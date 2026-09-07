//! A loopback provider and a temporary workspace, for driving the real `rivet` binary.
//!
//! The end-to-end tests spawn `rivet` as a subprocess, which rules out an in-process model
//! double: the child has to reach a model over the network like any other run. So the test
//! stands up its own `OpenAI`-compatible SSE endpoint on a loopback port and points the
//! child's `rivet.toml` at it.
//!
//! The server writes every request body to a file. That is what makes the interesting
//! assertions possible from outside the subprocess: whether the model was shown the tool
//! result it asked for, and whether a resumed run carries the synthetic result forward.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Environment variable the generated config points `api_key_env` at.
pub const KEY_ENV: &str = "RIVET_TEST_KEY";

// --- SSE bodies ---------------------------------------------------------------------------

/// A plain text answer.
#[must_use]
pub fn sse_text(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\
         \"content\":{}}},\"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\
         \"usage\":{{\"prompt_tokens\":12,\"completion_tokens\":4}}}}\n\n\
         data: [DONE]\n\n",
        serde_json::Value::String(text.to_string())
    )
}

/// A single tool call.
#[must_use]
pub fn sse_tool_call(name: &str, arguments: &serde_json::Value) -> String {
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\
         \"tool_calls\":[{{\"index\":0,\"id\":\"call_0\",\"type\":\"function\",\
         \"function\":{{\"name\":\"{name}\",\"arguments\":{}}}}}]}},\
         \"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}],\
         \"usage\":{{\"prompt_tokens\":12,\"completion_tokens\":4}}}}\n\n\
         data: [DONE]\n\n",
        serde_json::Value::String(arguments.to_string())
    )
}

// --- the server ----------------------------------------------------------------------------

/// A loopback `OpenAI`-compatible endpoint that answers a fixed script.
#[derive(Debug)]
pub struct Provider {
    pub base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Provider {
    /// Serve `script` in order; after it runs out, answer with a short final message.
    pub async fn start(script: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));

        let handle = tokio::spawn({
            let requests = requests.clone();
            async move {
                let mut remaining = script.into_iter();
                let tail = sse_text("done");
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        return;
                    };
                    let body = remaining.next().unwrap_or_else(|| tail.clone());
                    let requests = requests.clone();
                    // Serve each connection on its own task: a killed client must not
                    // stop the server from answering the resumed run.
                    tokio::spawn(async move {
                        let _ = serve(socket, &body, &requests).await;
                    });
                }
            }
        });

        Self {
            base_url: format!("http://{addr}/v1"),
            requests,
            handle,
        }
    }

    /// Request bodies received so far, in order.
    pub async fn requests(&self) -> Vec<serde_json::Value> {
        self.requests
            .lock()
            .await
            .iter()
            .filter_map(|body| serde_json::from_str(body).ok())
            .collect()
    }

    /// Wait until at least `n` requests have arrived.
    pub async fn wait_for_requests(&self, n: usize, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.requests.lock().await.len() >= n {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        false
    }
}

async fn serve(
    socket: tokio::net::TcpStream,
    body: &str,
    requests: &Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
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

    let mut raw = vec![0u8; content_length];
    reader.read_exact(&mut raw).await?;
    requests
        .lock()
        .await
        .push(String::from_utf8_lossy(&raw).into_owned());

    write
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Cache-Control: no-cache\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    write.write_all(body.as_bytes()).await?;
    write.flush().await?;
    Ok(())
}

// --- the workspace ---------------------------------------------------------------------------

/// A temporary workspace with a `rivet.toml` pointing at the loopback provider.
#[derive(Debug)]
pub struct Workspace {
    pub dir: tempfile::TempDir,
}

impl Workspace {
    pub fn new(base_url: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("rivet.toml"),
            format!(
                "[agent]\n\
                 model = \"loopback/test-model\"\n\n\
                 [agent.limits]\n\
                 max_turns = 6\n\
                 max_duration_ms = 60000\n\
                 max_total_tokens = 100000\n\
                 max_context_tokens = 100000\n\
                 max_consecutive_tool_errors = 3\n\n\
                 [workspace]\n\
                 deny = [\".env\"]\n\n\
                 [plugins]\n\
                 enabled = [\"rivet.model-openai\", \"rivet.tool-filesystem\"]\n\n\
                 [plugins.\"rivet.model-openai\"]\n\
                 base_url = \"{base_url}\"\n\
                 api_key_env = \"{KEY_ENV}\"\n\n\
                 [policy]\n\
                 profile = \"developer\"\n"
            ),
        )
        .expect("write config");

        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"the marker string\");\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".env"), "TOKEN=secret\n").unwrap();

        Self { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Rewrite `[plugins].enabled` in this workspace's config.
    pub fn enable_plugins(&self, ids: &[&str]) {
        let path = self.dir.path().join("rivet.toml");
        let config = std::fs::read_to_string(&path).expect("read config");
        let list = ids
            .iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let replaced = config
            .lines()
            .map(|line| {
                if line.starts_with("enabled = ") {
                    format!("enabled = [{list}]")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, replaced + "\n").expect("write config");
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.dir.path().join(".rivet/sessions")
    }

    /// The single session this workspace has, if it has one.
    pub fn session_log(&self) -> Option<PathBuf> {
        let entries = std::fs::read_dir(self.sessions_dir()).ok()?;
        entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.path().join("log.jsonl"))
            .find(|p| p.exists())
    }

    pub fn session_id(&self) -> Option<String> {
        let entries = std::fs::read_dir(self.sessions_dir()).ok()?;
        entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .next()
    }

    /// The session event names written so far.
    pub fn session_events(&self) -> Vec<String> {
        let Some(log) = self.session_log() else {
            return Vec::new();
        };
        let Ok(text) = std::fs::read_to_string(log) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|value| value["event"]["type"].as_str().map(str::to_string))
            .collect()
    }

    /// Wait until the session log contains `event`.
    pub async fn wait_for_event(&self, event: &str, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.session_events().iter().any(|e| e == event) {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        false
    }

    /// Build a `rivet` command rooted in this workspace.
    pub fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_rivet"));
        command
            .args(args)
            .current_dir(self.dir.path())
            .env(KEY_ENV, "not-a-real-key")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A test that panics drops its `Child` without waiting, and tokio does not
            // kill on drop by default. The tests that park a run on a named pipe would
            // then leave a `rivet` behind, blocked on a FIFO nobody will ever write to,
            // reparented to init and outliving the run that spawned it. One such orphan
            // survived a failing assertion here for half an hour before anyone noticed.
            .kill_on_drop(true);
        command
    }

    /// Run `rivet` to completion and return `(code, stdout, stderr)`.
    pub async fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = self.command(args).output().await.expect("spawn rivet");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

/// The tool result texts a recorded request body carries.
#[must_use]
pub fn tool_results(request: &serde_json::Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .filter(|m| m["role"] == "tool")
                .filter_map(|m| m["content"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
