//! `list_dir`: what is in a directory, without following anything out of the workspace.

use std::fmt::Write as _;

use async_trait::async_trait;
use rivet_core::tool::{Tool, ToolAnnotations, ToolContext, ToolResult, ToolSpec};
use rivet_runtime::fsguard::{self, EntryKind};

use crate::walk::{self, Limits};

/// Entries one call will list before it stops being useful.
const MAX_ENTRIES: usize = 500;
/// Deepest `depth` a caller may ask for.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Default)]
pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "list_dir",
            "List the entries of a directory inside the workspace. Symbolic links are \
             reported but never followed. Use `.` for the workspace root.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory relative to the workspace root. Defaults to `.`."
                    },
                    "depth": {
                        "type": "integer",
                        "description": "How many levels to descend. 1 lists only this directory.",
                        "minimum": 1,
                        "maximum": 8
                    }
                },
                "additionalProperties": false
            }),
        )
        .expect("the list_dir spec is valid")
        .with_annotations(ToolAnnotations {
            read_only: true,
            idempotent: true,
            expected_duration_ms: Some(100),
            ..ToolAnnotations::default()
        })
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let path = input["path"].as_str().unwrap_or(".");
        let depth = usize::try_from(input["depth"].as_u64().unwrap_or(1))
            .unwrap_or(1)
            .clamp(1, MAX_DEPTH);

        // The caller named this directory, so a refusal is raised rather than swallowed.
        let root = fsguard::resolve_dir(ctx.workspace(), std::path::Path::new(path))?;
        let workspace = ctx.workspace().clone();
        let walked = {
            let ctx = ctx.clone();
            tokio::task::spawn_blocking(move || {
                walk::walk(
                    &ctx,
                    &root,
                    Limits {
                        max_depth: depth,
                        max_entries: MAX_ENTRIES,
                    },
                )
            })
            .await
            .map_err(|e| {
                rivet_core::Error::internal("the directory walk did not finish").with_cause(e)
            })?
        };

        if walked.cancelled {
            return Err(rivet_core::Error::cancelled("listing was cancelled"));
        }

        let mut lines = Vec::with_capacity(walked.found.len());
        for found in &walked.found {
            let indent = "  ".repeat(found.depth);
            let rendered = match found.kind {
                EntryKind::Dir => format!("{indent}{}/", found.relative),
                EntryKind::Symlink => {
                    format!("{indent}{} -> (symlink, not followed)", found.relative)
                }
                EntryKind::File => match found.size {
                    Some(size) => format!("{indent}{} ({size} bytes)", found.relative),
                    None => format!("{indent}{}", found.relative),
                },
                EntryKind::Other => format!("{indent}{} (other)", found.relative),
            };
            lines.push(rendered);
        }

        let root_label = std::path::Path::new(path).display().to_string();
        let mut content = if lines.is_empty() {
            format!("`{root_label}` is empty (entries hidden by the deny list are not shown)")
        } else {
            lines.join("\n")
        };
        if walked.truncated {
            let _ = write!(content, "\n… [stopped after {MAX_ENTRIES} entries]");
        }

        Ok(ToolResult::ok(content).with_structured(serde_json::json!({
            "path": root_label,
            "entries": walked.found.len(),
            "workspace_root": workspace.root().display().to_string(),
            "truncated": walked.truncated
        })))
    }
}
