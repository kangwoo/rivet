//! `read_file`: read a text file, or say honestly that it is not one.

use std::fmt::Write as _;

use async_trait::async_trait;
use rivet_core::tool::{Tool, ToolAnnotations, ToolContext, ToolResult, ToolSpec, Truncation};
use rivet_runtime::fsguard;
use tokio::io::AsyncReadExt;

/// Largest file read in one call. Bigger files come back truncated with a note.
const MAX_BYTES: usize = 1_048_576;

/// How much of a file is inspected for NUL before calling it binary.
const SNIFF_BYTES: usize = 8_192;

#[derive(Debug, Default)]
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "read_file",
            "Read a UTF-8 text file inside the workspace. Use `offset` and `limit` to read \
             part of a large file; both count lines starting at 1. Binary files are \
             reported rather than dumped.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to the workspace root.",
                        "minLength": 1
                    },
                    "offset": {
                        "type": "integer",
                        "description": "First line to return, 1-based.",
                        "minimum": 1
                    },
                    "limit": {
                        "type": "integer",
                        "description": "How many lines to return.",
                        "minimum": 1
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
        .expect("the read_file spec is valid")
        .with_annotations(ToolAnnotations {
            read_only: true,
            idempotent: true,
            expected_duration_ms: Some(50),
            ..ToolAnnotations::default()
        })
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let path = input["path"].as_str().unwrap_or_default();
        let offset = usize::try_from(input["offset"].as_u64().unwrap_or(1)).unwrap_or(1);
        let limit = input["limit"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok());

        // A path the caller named: a denial here is raised, not swallowed. The model
        // asked for this file specifically and needs to know it was refused.
        let (mut file, opened) =
            fsguard::open_read(ctx.workspace(), std::path::Path::new(path)).await?;

        let mut bytes = Vec::new();
        // `take` rather than a size check first: the file could grow between the two.
        let bytes_read = (&mut file)
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| {
                rivet_core::Error::new(
                    rivet_core::error::ErrorKind::Storage,
                    rivet_core::error::Capability::Tool,
                    format!("could not read `{}`", opened.display()),
                )
                .with_cause(e)
            })?;

        if bytes[..bytes_read.min(SNIFF_BYTES)].contains(&0) {
            // Not an error the model can fix, and not something to paste into a prompt.
            return Ok(ToolResult::ok(format!(
                "`{path}` is a binary file ({bytes_read} bytes read); its contents are not shown."
            )));
        }

        let truncated_by_size = bytes_read > MAX_BYTES;
        bytes.truncate(bytes_read.min(MAX_BYTES));
        let text = String::from_utf8_lossy(&bytes).into_owned();

        let lines: Vec<&str> = text.lines().collect();
        let start = offset.saturating_sub(1).min(lines.len());
        let end = limit.map_or(lines.len(), |n| (start + n).min(lines.len()));
        let selected = lines[start..end].join("\n");
        let windowed = start > 0 || end < lines.len();

        let mut result = ToolResult::ok(selected).with_structured(serde_json::json!({
            "path": path,
            "lines": lines.len(),
            "returned_lines": end - start,
            "first_line": start + 1
        }));
        if truncated_by_size {
            result.truncated = Some(Truncation {
                original_bytes: bytes_read as u64,
                retained_bytes: MAX_BYTES as u64,
                artifact_ref: None,
            });
            let _ = write!(
                result.content,
                "\n… [`{path}` is larger than {MAX_BYTES} bytes; only the beginning is shown]"
            );
        } else if windowed {
            let _ = write!(
                result.content,
                "\n… [lines {}-{} of {}]",
                start + 1,
                end,
                lines.len()
            );
        }
        Ok(result)
    }
}
