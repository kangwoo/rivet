//! Shared helpers for the adapter's offline tests.

// Each test binary uses a different subset of this module.
#![allow(dead_code)]

use std::sync::Arc;

use futures_util::StreamExt;
use rivet_core::model::StreamEvent;
use rivet_model_openai::{SequentialIds, decode_recorded_stream};

/// Load a recorded response by name from `tests/fixtures`.
pub fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Decode a fixture with deterministic tool-call ids.
///
/// Returns every item, errors included, so a test can assert on where a stream failed.
pub async fn decode(name: &str) -> Vec<rivet_core::Result<StreamEvent>> {
    let body = fixture(name);
    decode_recorded_stream(&body, Arc::new(SequentialIds::new()), 100)
        .collect()
        .await
}

/// The terminal event, or a panic naming what arrived instead.
pub fn done(events: &[rivet_core::Result<StreamEvent>]) -> &StreamEvent {
    match events.last() {
        Some(Ok(event @ StreamEvent::Done { .. })) => event,
        other => panic!("expected a Done at the end of the stream, got {other:?}"),
    }
}

/// A minimal HTTP/1.1 server that speaks `text/event-stream`.
///
/// It exists so the adapter's real `reqwest` path -- headers, chunked framing, streaming
/// body, connection teardown -- is exercised offline. A mocking crate would test the mock;
/// this tests the client. It also makes "dropping the stream aborts the request"
/// observable **from the server side**, which is half of the cancellation requirement.
pub mod loopback {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    /// What the server should answer with, once per request.
    #[derive(Clone, Debug)]
    pub enum Reply {
        /// A `200` with this body, written in one go.
        Sse(String),
        /// A `200` whose body dribbles out, so the client can be cancelled mid-stream.
        Dripping { frames: Vec<String>, delay_ms: u64 },
        /// A non-2xx response with these extra headers.
        Status {
            code: u16,
            headers: Vec<(String, String)>,
            body: String,
        },
    }

    /// A running loopback endpoint.
    #[derive(Debug)]
    pub struct LoopbackServer {
        pub base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
        client_hung_up: Arc<AtomicBool>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for LoopbackServer {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    impl LoopbackServer {
        /// Bind an ephemeral port and answer `replies` in order.
        pub async fn start(replies: Vec<Reply>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("addr");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let client_hung_up = Arc::new(AtomicBool::new(false));

            let handle = tokio::spawn({
                let requests = requests.clone();
                let hung_up = client_hung_up.clone();
                async move {
                    for reply in replies {
                        let Ok((socket, _)) = listener.accept().await else {
                            return;
                        };
                        if let Err(e) = serve_one(socket, &reply, &requests).await
                            && matches!(
                                e.kind(),
                                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                            )
                        {
                            hung_up.store(true, Ordering::SeqCst);
                        }
                    }
                }
            });

            Self {
                base_url: format!("http://{addr}/v1"),
                requests,
                client_hung_up,
                handle,
            }
        }

        /// The request bodies received so far, in order.
        pub async fn requests(&self) -> Vec<String> {
            self.requests.lock().await.clone()
        }

        /// Whether a client disconnected while the server was still writing.
        pub fn client_hung_up(&self) -> bool {
            self.client_hung_up.load(Ordering::SeqCst)
        }
    }

    async fn serve_one(
        socket: tokio::net::TcpStream,
        reply: &Reply,
        requests: &Arc<Mutex<Vec<String>>>,
    ) -> io::Result<()> {
        let (read_half, mut write) = socket.into_split();
        let mut reader = BufReader::new(read_half);

        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed
                .strip_prefix("content-length:")
                .or_else(|| trimmed.strip_prefix("Content-Length:"))
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }

        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body).await?;
        requests
            .lock()
            .await
            .push(String::from_utf8_lossy(&body).into_owned());

        match reply {
            Reply::Sse(body) => {
                write.write_all(sse_headers(body.len()).as_bytes()).await?;
                write.write_all(body.as_bytes()).await?;
                write.flush().await?;
            }
            Reply::Dripping { frames, delay_ms } => {
                // No Content-Length: the body ends when the connection does, which is what
                // lets the client hang up early and the server notice.
                write
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                          Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
                    )
                    .await?;
                write.flush().await?;
                for frame in frames {
                    tokio::time::sleep(std::time::Duration::from_millis(*delay_ms)).await;
                    write.write_all(frame.as_bytes()).await?;
                    write.flush().await?;
                }
            }
            Reply::Status {
                code,
                headers,
                body,
            } => {
                use std::fmt::Write as _;
                let mut response = format!("HTTP/1.1 {code} Error\r\n");
                for (name, value) in headers {
                    let _ = write!(response, "{name}: {value}\r\n");
                }
                let _ = write!(
                    response,
                    "Content-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n",
                    body.len()
                );
                write.write_all(response.as_bytes()).await?;
                write.write_all(body.as_bytes()).await?;
                write.flush().await?;
            }
        }
        Ok(())
    }

    fn sse_headers(len: usize) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
             Cache-Control: no-cache\r\nContent-Length: {len}\r\n\
             Connection: close\r\n\r\n"
        )
    }
}
