//! `write_file`: replace a file's contents, atomically.

use async_trait::async_trait;
use rivet_core::tool::{Tool, ToolAnnotations, ToolContext, ToolResult, ToolSpec};
use rivet_runtime::fsguard;

#[derive(Debug, Default)]
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "write_file",
            "Write a UTF-8 text file inside the workspace, replacing it if it exists. The \
             parent directory must already exist. The write is atomic: readers see either \
             the old file or the new one.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to the workspace root.",
                        "minLength": 1
                    },
                    "content": {
                        "type": "string",
                        "description": "The complete new contents of the file."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        )
        .expect("the write_file spec is valid")
        .with_annotations(ToolAnnotations {
            destructive: true,
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
        let content = input["content"].as_str().unwrap_or_default().to_string();
        let bytes = content.len();

        // No `mkdir -p`: creating directories widens the path surface for no benefit, and
        // a write to a directory that does not exist is nearly always a wrong path.
        let written = fsguard::write_atomic(
            ctx.workspace(),
            std::path::Path::new(path),
            content.into_bytes(),
        )
        .await?;

        let relative = written
            .strip_prefix(ctx.workspace().root())
            .unwrap_or(&written)
            .to_string_lossy()
            .into_owned();
        Ok(
            ToolResult::ok(format!("wrote {bytes} bytes to `{relative}`"))
                .with_structured(serde_json::json!({ "path": relative, "bytes": bytes })),
        )
    }
}
