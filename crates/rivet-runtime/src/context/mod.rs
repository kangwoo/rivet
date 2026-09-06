//! Context assembly: providers in, one budgeted [`AssembledContext`] out.
//!
//! Three properties, each of which a naive implementation loses:
//!
//! 1. **Order is declaration order, not completion order.** Providers run concurrently but
//!    their items are concatenated in the order they were declared. A prompt whose shape
//!    depends on which provider finished first breaks prompt caching on every turn.
//! 2. **A failing provider does not kill the turn.** It contributes nothing and the
//!    failure is logged. The one item that must never be lost — the system prompt — comes
//!    from a provider that only concatenates strings and therefore cannot fail.
//! 3. **History is dropped in turn groups**, never message by message. See
//!    [`history`].

pub mod history;
pub mod providers;

use std::fmt;
use std::sync::Arc;

use rivet_core::context::{
    AssembledContext, ContextItem, ContextProvider, ContextRequest, DroppedItem, estimate_tokens,
    fit_to_budget,
};
use rivet_core::model::Message;

/// The assembled request cannot be made to fit, at any priority.
///
/// A distinct type rather than an [`rivet_core::Error`] so the loop's mapping to
/// `StopReason::LimitReached { ContextSize }` is total: there is no other way for
/// assembly to fail.
#[derive(Clone, Debug)]
pub struct ContextOverflow {
    pub budget_tokens: u32,
    pub needed_tokens: u32,
    pub detail: String,
}

impl fmt::Display for ContextOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "context budget is {} tokens but {} are required: {}",
            self.budget_tokens, self.needed_tokens, self.detail
        )
    }
}

/// Assembles context from a fixed, ordered list of providers.
#[derive(Clone)]
pub struct ContextAssembler {
    providers: Vec<Arc<dyn ContextProvider>>,
}

impl fmt::Debug for ContextAssembler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextAssembler")
            .field(
                "providers",
                &self
                    .providers
                    .iter()
                    .map(|p| p.name().to_string())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ContextAssembler {
    #[must_use]
    pub fn new(providers: Vec<Arc<dyn ContextProvider>>) -> Self {
        Self { providers }
    }

    /// Select providers by name, in the order an [`rivet_core::agent::AgentSpec`] asked
    /// for.
    ///
    /// An empty list means "every registered provider", which is what an interactive agent
    /// wants. A non-empty list is an *assembly order*, and a name in it that nothing
    /// registered is a startup failure: quietly skipping it would run an agent that was
    /// configured to have context without any.
    ///
    /// # Errors
    /// [`rivet_core::error::ErrorKind::NotFound`] naming the missing provider and listing
    /// the ones that exist.
    pub fn for_agent(
        available: Vec<Arc<dyn ContextProvider>>,
        wanted: &[String],
    ) -> rivet_core::Result<Self> {
        if wanted.is_empty() {
            return Ok(Self::new(available));
        }
        let mut selected = Vec::with_capacity(wanted.len());
        for name in wanted {
            let found = available.iter().find(|p| p.name() == name).ok_or_else(|| {
                rivet_core::Error::not_found(format!(
                    "context provider `{name}` is not registered; available: {}",
                    available
                        .iter()
                        .map(|p| p.name().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            selected.push(found.clone());
        }
        Ok(Self::new(selected))
    }

    /// The provider names, in assembly order.
    #[must_use]
    pub fn provider_names(&self) -> Vec<String> {
        self.providers
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }

    /// Assemble a system prompt and a trimmed message list.
    ///
    /// `pinned_history` is the number of leading messages that must survive trimming —
    /// `1` when a checkpoint has folded earlier history into the first message, `0`
    /// otherwise.
    ///
    /// # Errors
    /// [`ContextOverflow`] when the required items, or the newest turn, do not fit
    /// `request.budget_tokens`.
    pub async fn assemble(
        &self,
        request: &ContextRequest,
        history: &[Message],
        pinned_history: usize,
    ) -> Result<AssembledContext, ContextOverflow> {
        let budget = request.budget_tokens;
        let groups = history::group(history);
        let floor = history::floor_tokens(&groups);

        // `checked_sub`, not `saturating_sub`: wrapping here would produce an enormous
        // budget and a request that overflows the window after we said it fit.
        let Some(item_budget) = budget.checked_sub(floor) else {
            return Err(ContextOverflow {
                budget_tokens: budget,
                needed_tokens: floor,
                detail: "the newest turn alone exceeds max_context_tokens".into(),
            });
        };

        let items = self.collect_items(request).await;
        let (kept, dropped) = match fit_to_budget(items, item_budget) {
            Ok(pair) => pair,
            Err(err) => {
                return Err(ContextOverflow {
                    budget_tokens: budget,
                    needed_tokens: item_budget,
                    detail: err.message().to_string(),
                });
            }
        };

        let system = render(&kept);
        let system_tokens = estimate_tokens(&system);
        let Some(history_budget) = budget.checked_sub(system_tokens) else {
            return Err(ContextOverflow {
                budget_tokens: budget,
                needed_tokens: system_tokens,
                detail: "the system prompt alone exceeds max_context_tokens".into(),
            });
        };

        let Some(trimmed) = history::trim(history, history_budget, pinned_history) else {
            return Err(ContextOverflow {
                budget_tokens: budget,
                needed_tokens: system_tokens.saturating_add(floor),
                detail: "no turn of history fits beside the system prompt".into(),
            });
        };

        Ok(AssembledContext {
            system,
            messages: trimmed.messages,
            dropped: dropped_report(dropped, trimmed.dropped_groups),
            estimated_tokens: system_tokens.saturating_add(trimmed.tokens),
        })
    }

    /// Run every provider, concurrently, and concatenate in declaration order.
    async fn collect_items(&self, request: &ContextRequest) -> Vec<ContextItem> {
        let results =
            futures_util::future::join_all(self.providers.iter().map(|p| p.provide(request))).await;

        let mut items = Vec::new();
        for (provider, result) in self.providers.iter().zip(results) {
            match result {
                Ok(mut produced) => items.append(&mut produced),
                // A failing provider costs its own contribution and nothing else. There is
                // no `RuntimeEvent` variant for this and inventing one would be a contract
                // change, so it is reported here.
                Err(err) => tracing::warn!(
                    provider = provider.name(),
                    error = %err,
                    "context provider failed; continuing without its items"
                ),
            }
        }
        items
    }
}

/// Concatenate kept items into the system prompt.
///
/// `fit_to_budget` already returns them in slot order, so this only joins.
fn render(items: &[ContextItem]) -> String {
    items
        .iter()
        .map(|item| item.content.trim_end())
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Report dropped history alongside dropped items, so a UI can warn about both.
fn dropped_report(mut dropped: Vec<DroppedItem>, dropped_groups: usize) -> Vec<DroppedItem> {
    if dropped_groups > 0 {
        dropped.push(DroppedItem {
            key: format!("history.turns:{dropped_groups}"),
            slot: rivet_core::context::ContextSlot::History,
            tokens: 0,
        });
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rivet_core::context::{ContextSlot, Priority};
    use rivet_core::id::{AgentId, RunId, SessionId};
    use rivet_core::workspace::Workspace;

    #[derive(Debug)]
    struct Fixed {
        name: &'static str,
        items: Vec<ContextItem>,
        fail: bool,
    }

    #[async_trait]
    impl ContextProvider for Fixed {
        fn name(&self) -> &str {
            self.name
        }

        async fn provide(&self, _request: &ContextRequest) -> rivet_core::Result<Vec<ContextItem>> {
            if self.fail {
                return Err(rivet_core::Error::internal("provider exploded"));
            }
            Ok(self.items.clone())
        }
    }

    fn item(slot: ContextSlot, key: &str, content: &str, priority: Priority) -> ContextItem {
        ContextItem::new(slot, key, content).with_priority(priority)
    }

    fn request(budget: u32) -> ContextRequest {
        ContextRequest {
            session_id: SessionId::new(),
            agent_id: AgentId::new(),
            run_id: RunId::new(),
            job_id: None,
            workspace: Workspace::new(std::path::PathBuf::from("/repo")),
            turn: 0,
            budget_tokens: budget,
        }
    }

    fn assembler(providers: Vec<Fixed>) -> ContextAssembler {
        ContextAssembler::new(
            providers
                .into_iter()
                .map(|p| Arc::new(p) as Arc<dyn ContextProvider>)
                .collect(),
        )
    }

    #[tokio::test]
    async fn items_appear_in_declaration_order_not_completion_order() {
        // A prompt whose shape depends on which provider won the race breaks prompt
        // caching on every single turn.
        let a = assembler(vec![
            Fixed {
                name: "system",
                items: vec![item(
                    ContextSlot::SystemPrompt,
                    "system.instructions",
                    "you are careful",
                    Priority::Required,
                )],
                fail: false,
            },
            Fixed {
                name: "workspace",
                items: vec![item(
                    ContextSlot::Environment,
                    "workspace.tree",
                    "src/",
                    Priority::Normal,
                )],
                fail: false,
            },
        ]);
        let assembled = a.assemble(&request(1000), &[], 0).await.unwrap();
        assert_eq!(assembled.system, "you are careful\n\nsrc/");
        assert_eq!(a.provider_names(), ["system", "workspace"]);
    }

    #[tokio::test]
    async fn a_failing_provider_does_not_kill_the_turn() {
        let a = assembler(vec![
            Fixed {
                name: "broken",
                items: Vec::new(),
                fail: true,
            },
            Fixed {
                name: "system",
                items: vec![item(
                    ContextSlot::SystemPrompt,
                    "system.instructions",
                    "still here",
                    Priority::Required,
                )],
                fail: false,
            },
        ]);
        let assembled = a.assemble(&request(1000), &[], 0).await.unwrap();
        assert_eq!(assembled.system, "still here");
    }

    #[tokio::test]
    async fn a_required_item_larger_than_the_budget_is_reported_not_shipped() {
        let a = assembler(vec![Fixed {
            name: "system",
            items: vec![item(
                ContextSlot::SystemPrompt,
                "system.instructions",
                &"word ".repeat(5000),
                Priority::Required,
            )],
            fail: false,
        }]);
        let err = a.assemble(&request(100), &[], 0).await.unwrap_err();
        assert!(err.detail.contains("required context"), "{err}");
    }

    #[tokio::test]
    async fn history_is_reserved_before_items_are_admitted() {
        // The last turn is a floor: items must not eat the budget the conversation needs.
        let a = assembler(vec![Fixed {
            name: "workspace",
            items: vec![item(
                ContextSlot::Environment,
                "workspace.tree",
                &"tree ".repeat(200),
                Priority::Normal,
            )],
            fail: false,
        }]);
        let history = vec![Message::user("what changed?")];
        let assembled = a.assemble(&request(60), &history, 0).await.unwrap();
        assert_eq!(assembled.messages.len(), 1, "the newest turn survives");
        assert!(
            assembled.system.is_empty(),
            "the oversized optional item was dropped instead"
        );
        assert!(assembled.dropped.iter().any(|d| d.key == "workspace.tree"));
    }

    #[tokio::test]
    async fn dropped_history_is_reported() {
        let a = assembler(vec![]);
        let history = vec![
            Message::user("old ".repeat(200)),
            Message::assistant("recent"),
        ];
        let assembled = a.assemble(&request(60), &history, 0).await.unwrap();
        assert_eq!(assembled.messages.len(), 1);
        assert!(
            assembled
                .dropped
                .iter()
                .any(|d| d.key.starts_with("history.turns:")),
            "a UI must be able to say what was cut: {:?}",
            assembled.dropped
        );
    }

    #[tokio::test]
    async fn an_agent_can_choose_its_providers_and_their_order() {
        let available: Vec<Arc<dyn ContextProvider>> = vec![
            Arc::new(Fixed {
                name: "system",
                items: Vec::new(),
                fail: false,
            }),
            Arc::new(Fixed {
                name: "workspace",
                items: Vec::new(),
                fail: false,
            }),
        ];
        let chosen = ContextAssembler::for_agent(
            available.clone(),
            &["workspace".to_string(), "system".to_string()],
        )
        .unwrap();
        assert_eq!(chosen.provider_names(), ["workspace", "system"]);

        let all = ContextAssembler::for_agent(available.clone(), &[]).unwrap();
        assert_eq!(all.provider_names(), ["system", "workspace"]);
    }

    #[test]
    fn a_provider_an_agent_asked_for_but_nothing_registered_is_a_startup_failure() {
        // Skipping it silently runs a reviewer with no context at all, which looks like a
        // bad reviewer rather than a bad configuration.
        let available: Vec<Arc<dyn ContextProvider>> = vec![Arc::new(Fixed {
            name: "system",
            items: Vec::new(),
            fail: false,
        })];
        let err = ContextAssembler::for_agent(available, &["git".to_string()]).unwrap_err();
        assert_eq!(err.kind(), rivet_core::error::ErrorKind::NotFound);
        assert!(err.message().contains("available: system"), "{err}");
    }

    #[tokio::test]
    async fn assembly_is_deterministic() {
        let build = || {
            assembler(vec![Fixed {
                name: "system",
                items: vec![
                    item(
                        ContextSlot::Environment,
                        "env",
                        "environment",
                        Priority::Normal,
                    ),
                    item(
                        ContextSlot::SystemPrompt,
                        "sys",
                        "instructions",
                        Priority::Required,
                    ),
                ],
                fail: false,
            }])
        };
        let first = build().assemble(&request(1000), &[], 0).await.unwrap();
        let second = build().assemble(&request(1000), &[], 0).await.unwrap();
        assert_eq!(first.system, second.system);
        assert_eq!(
            first.system, "instructions\n\nenvironment",
            "slot order, not declaration order, decides the layout"
        );
    }
}
