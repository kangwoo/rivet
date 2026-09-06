//! `search`: fixed-string search across the workspace.
//!
//! Fixed strings, not regular expressions. A regex engine is a new dependency and a
//! denial-of-service surface (a pathological pattern on a large tree), and neither is
//! justified by the Phase 1 acceptance criteria. File selection still gets a glob, which
//! is where the expressiveness is usually wanted.
//!
//! This is the only long-running tool Phase 1 ships, so it is also where cancellation is
//! most visible: the loop checks are in the walk and in the per-file scan.

use std::fmt::Write as _;

use async_trait::async_trait;
use globset::{Glob, GlobMatcher};
use rivet_core::tool::{Tool, ToolAnnotations, ToolContext, ToolResult, ToolSpec};
use rivet_runtime::fsguard::{self, EntryKind};

use crate::walk::{self, Limits};

/// Files examined before the search gives up on being exhaustive.
const MAX_FILES: usize = 5_000;
/// How deep the search descends.
const MAX_DEPTH: usize = 20;
/// Files larger than this are skipped: they are data, not source.
const MAX_FILE_BYTES: u64 = 1_048_576;
/// Default and maximum number of matches returned.
const DEFAULT_RESULTS: usize = 100;
const MAX_RESULTS: usize = 1_000;
/// A matching line longer than this is cut; a minified bundle is not a useful hit.
const MAX_LINE_CHARS: usize = 300;

#[derive(Debug, Default)]
pub struct Search;

#[async_trait]
impl Tool for Search {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "search",
            "Find a literal string in the workspace's text files. This is a plain substring \
             search, not a regular expression. Narrow it with `glob` (for example \
             `**/*.rs`) and `path`.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The literal text to find.",
                        "minLength": 1
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search under. Defaults to the workspace root."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Only search files whose path matches this glob."
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "description": "Defaults to false."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 1000
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
        .expect("the search spec is valid")
        .with_annotations(ToolAnnotations {
            read_only: true,
            idempotent: true,
            expected_duration_ms: Some(2_000),
            ..ToolAnnotations::default()
        })
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        input: serde_json::Value,
    ) -> rivet_core::Result<ToolResult> {
        let query = input["query"].as_str().unwrap_or_default().to_string();
        let path = input["path"].as_str().unwrap_or(".").to_string();
        let case_sensitive = input["case_sensitive"].as_bool().unwrap_or(false);
        let max_results = input["max_results"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(DEFAULT_RESULTS)
            .clamp(1, MAX_RESULTS);

        let matcher = match input["glob"].as_str() {
            Some(pattern) => Some(Glob::new(pattern).map(|g| g.compile_matcher()).map_err(
                |e| {
                    rivet_core::Error::invalid_argument(format!("bad glob `{pattern}`"))
                        .with_cause(e)
                },
            )?),
            None => None,
        };

        // The caller named this directory, so a refusal propagates.
        let root = fsguard::resolve_dir(ctx.workspace(), std::path::Path::new(&path))?;

        let scan_ctx = ctx.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            scan(
                &scan_ctx,
                &root,
                &query,
                case_sensitive,
                matcher.as_ref(),
                max_results,
            )
        })
        .await
        .map_err(|e| rivet_core::Error::internal("the search did not finish").with_cause(e))?;

        if outcome.cancelled {
            return Err(rivet_core::Error::cancelled("search was cancelled"));
        }

        let content = if outcome.hits.is_empty() {
            format!("no matches for `{}`", outcome.query)
        } else {
            let mut text = outcome.hits.join("\n");
            if outcome.capped {
                let _ = write!(text, "\n… [stopped after {max_results} matches]");
            } else if outcome.walk_truncated {
                let _ = write!(text, "\n… [stopped after {MAX_FILES} files]");
            }
            text
        };

        Ok(ToolResult::ok(content).with_structured(serde_json::json!({
            "query": outcome.query,
            "matches": outcome.hit_count,
            "files_scanned": outcome.files_scanned,
            "complete": !outcome.capped && !outcome.walk_truncated
        })))
    }
}

#[derive(Debug, Default)]
struct Outcome {
    query: String,
    hits: Vec<String>,
    hit_count: usize,
    files_scanned: usize,
    capped: bool,
    walk_truncated: bool,
    cancelled: bool,
}

/// Walk and scan. Blocking; called from `spawn_blocking`.
fn scan(
    ctx: &ToolContext,
    root: &std::path::Path,
    query: &str,
    case_sensitive: bool,
    matcher: Option<&GlobMatcher>,
    max_results: usize,
) -> Outcome {
    let walked = walk::walk(
        ctx,
        root,
        Limits {
            max_depth: MAX_DEPTH,
            max_entries: MAX_FILES,
        },
    );

    let mut outcome = Outcome {
        query: query.to_string(),
        walk_truncated: walked.truncated,
        cancelled: walked.cancelled,
        ..Outcome::default()
    };
    if outcome.cancelled {
        return outcome;
    }

    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };

    for found in walked.found {
        // The natural place to notice a Ctrl-C: this loop is the long part of the run.
        if ctx.host.is_cancelled() {
            outcome.cancelled = true;
            return outcome;
        }
        if found.kind != EntryKind::File || found.size.is_some_and(|s| s > MAX_FILE_BYTES) {
            continue;
        }
        if let Some(matcher) = matcher
            && !matcher.is_match(&found.relative)
        {
            continue;
        }

        // Through the guard, not around it: `walk` classified this entry with
        // `symlink_metadata`, and between that call and this read the entry can be
        // swapped for a link pointing out of the workspace. `open_read_blocking` re-checks
        // after the open, which is the whole point of `fsguard`.
        let Ok((mut file, _)) = fsguard::open_read_blocking(ctx.workspace(), &found.path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(&mut file, &mut bytes).is_err() {
            continue;
        }
        if bytes.iter().take(8_192).any(|b| *b == 0) {
            continue;
        }
        outcome.files_scanned += 1;

        let text = String::from_utf8_lossy(&bytes);
        for (number, line) in text.lines().enumerate() {
            let haystack = if case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };
            if !haystack.contains(&needle) {
                continue;
            }
            outcome.hit_count += 1;
            if outcome.hits.len() >= max_results {
                outcome.capped = true;
                return outcome;
            }
            outcome.hits.push(format!(
                "{}:{}: {}",
                found.relative,
                number + 1,
                clip(line.trim_end())
            ));
        }
    }
    outcome
}

/// Keep a matching line readable, cutting on a character boundary.
fn clip(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(MAX_LINE_CHARS).collect();
    out.push('…');
    out
}
