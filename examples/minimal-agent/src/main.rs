//! The smallest useful exercise of the contracts: register a tool into a scoped registry,
//! resolve it, publish an event, and unload cleanly.
//!
//! This exists to keep the contracts honest. If adding a capability makes this example
//! awkward, the contract is wrong.

use std::sync::Arc;

use rivet_core::error::Result;
use rivet_core::event::{Event, EventBus, EventEnvelope, ToolEvent};
use rivet_core::id::{PluginId, PluginInstanceId, ToolCallId};
use rivet_core::plugin::PluginRegistry;
use rivet_core::tool::{Tool, ToolContext, ToolResult, ToolSpec};
use rivet_runtime::bus::BroadcastBus;
use rivet_runtime::registry::{Owner, Registry};

#[derive(Debug)]
struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "echo",
            "Return the `text` argument unchanged. Useful only for testing the runtime.",
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        )
        .expect("a literal spec must be valid")
    }

    async fn execute(&self, _ctx: ToolContext, input: serde_json::Value) -> Result<ToolResult> {
        let text = input
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Ok(ToolResult::ok(text))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let bus = BroadcastBus::new();
    let registry = Registry::new(bus.clone());

    let owner = Owner {
        plugin_id: PluginId::new("example.echo")?,
        instance_id: PluginInstanceId::new(),
    };

    registry
        .scoped(owner.clone())
        .register_tool(Arc::new(EchoTool))
        .await?;
    println!("registered tools: {:?}", registry.tool_names().await);

    let tool = registry.tool("echo").await.expect("just registered");
    println!("spec: {}", tool.spec().description);

    bus.publish(EventEnvelope::new(Event::Tool(ToolEvent::Requested {
        call_id: ToolCallId::new(),
        name: "echo".into(),
    })));
    println!("events published: {}", bus.published());

    let removed = registry.unregister_all(owner.instance_id).await;
    println!("unloaded: {removed:?}");
    assert!(registry.tool("echo").await.is_none());

    Ok(())
}
