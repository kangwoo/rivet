# Plugin 개발 가이드

> 대상: Rivet plugin 작성자 · 계약 버전 `0.1`

Plugin이 하는 일은 하나다: **넘겨받은 registry에 capability를 등록한다.**
런타임에 손을 뻗지 않고, 내부 상태를 바꾸지 않고, Agent Loop 참조를 들지 않는다.

이 제약이 같은 코드를 오늘은 in-process로, 나중에는 별도 프로세스나 WASM으로 실행할 수
있게 만든다.

---

## 1. 채울 수 있는 슬롯

| `CapabilityKind` | Trait | 하는 일 |
|---|---|---|
| `Model` | `Model` | LLM provider |
| `Tool` | `Tool` | 모델에게 제공되는 실행 능력 |
| `ContextProvider` | `ContextProvider` | 프롬프트에 들어갈 정보 |
| `Policy` | `Policy` | 허가 판단 |
| `Sandbox` | `Sandbox` | 실행 격리 |
| `SessionStore` | `SessionStore` | 세션 영속화 |
| `Memory` | `Memory` | 회상·기억 |
| `Workflow` | `Workflow` | Task 그래프 실행 정책 |
| `Scheduler` | `Scheduler` | READY task → Agent Run |
| `Evaluator` | `Evaluator` | 완료된 Run 채점 |
| `EventSubscriber` | `EventSubscriber` | 관찰 (차단 불가) |
| `Command` | — | CLI 하위 명령 (Phase 3) |

---

## 2. 매니페스트

`rivet-plugin.toml`:

```toml
[plugin]
id          = "rivet.tool-git"     # namespace.name, lowercase, 점 1개 이상
name        = "Git"
version     = "0.1.0"              # plugin 자신의 버전
abi_version = "0.1"                # 빌드된 계약 버전
description = "Git status, diff, log and commit tools."

capabilities = ["tool"]

[[permissions]]
permission = "fs_read"
scope      = "workspace"

[[permissions]]
permission = "process_spawn"
```

`abi_version`은 **등록 전에** 검사된다. 호환되지 않으면 plugin이 아무것도 등록하지
못한 채 거부된다.

major `0`은 Cargo와 같이 **모든 minor 변경을 breaking으로** 취급한다.
`0.1` 호스트는 `0.1` plugin만 받는다.

---

## 3. 최소 Plugin

```rust
use rivet_core::prelude::*;
use std::sync::Arc;

#[derive(Debug)]
struct HelloTool;

#[async_trait]
impl Tool for HelloTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "hello",
            // 설명은 사람이 아니라 *모델*을 위해 쓴다.
            // 언제 쓰는지, 그리고 언제 쓰면 안 되는지를 적는다.
            "Greet someone by name. Use only when the user explicitly asks for a greeting.",
            serde_json::json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ).expect("literal spec is valid")
    }

    async fn execute(&self, ctx: ToolContext, input: serde_json::Value) -> Result<ToolResult> {
        // 취소를 반드시 존중한다. 무시하면 Run 전체가 멈춘다.
        ctx.check_cancelled()?;
        let name = input["name"].as_str().unwrap_or("world");
        Ok(ToolResult::ok(format!("Hello, {name}!")))
    }
}

#[derive(Debug)]
pub struct HelloPlugin;

#[async_trait]
impl Plugin for HelloPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest::new(
            PluginId::new("example.hello").unwrap(),
            "Hello",
            "0.1.0",
        )
        .with_capabilities([CapabilityKind::Tool])
    }

    async fn load(&self, ctx: PluginContext) -> Result<PluginHandle> {
        // 필요한 권한이 거부됐다면 여기서 크게 실패한다.
        // 첫 tool call에서 실패하는 것보다 낫다.
        ctx.registry.register_tool(Arc::new(HelloTool)).await?;
        Ok(PluginHandle::new(["tool:hello".to_string()]))
    }

    async fn unload(&self, _ctx: PluginContext) -> Result<()> {
        // 등록 해제는 로더가 이미 했다. 여기서는 내가 시작한 것만 정리한다.
        Ok(())
    }
}
```

---

## 4. 지켜야 할 규칙

### 4.1 `load()`는 실패 시 아무것도 남기지 않는다

로더가 `unregister_all(instance_id)`로 등록을 되돌린다. 하지만 **직접 띄운 백그라운드
태스크는 로더가 모른다.** 에러 경로에서 반드시 스스로 정리한다.

```rust
async fn load(&self, ctx: PluginContext) -> Result<PluginHandle> {
    // shutdown 토큰을 넘겨 언로드 시 스스로 멈추게 한다.
    let shutdown = ctx.shutdown.clone();
    let worker = tokio::spawn(async move {
        tokio::select! {
            () = background_sync() => {}
            () = shutdown.cancelled() => {}
        }
    });

    if let Err(e) = ctx.registry.register_tool(Arc::new(MyTool)).await {
        worker.abort();          // ← 이것을 빠뜨리면 좀비가 남는다
        return Err(e);
    }
    Ok(PluginHandle::new(["tool:mine".to_string()]))
}
```

`PluginContext::shutdown`은 런타임 종료 또는 이 plugin 언로드 시 취소된다.
이벤트 구독 태스크는 런타임이 소유자별로 추적해 언로드 시 abort하지만, plugin이 직접 띄운
태스크는 plugin 책임이다.

### 4.2 권한은 넓힐 수 없다

```text
effective = manifest.permissions ∩ profile.permissions ∩ session overrides
```

`ctx.permissions`가 실제로 받은 것이다. 매니페스트에 적었다고 받은 게 아니다.

```rust
if !ctx.permissions.contains(&Permission::ProcessSpawn) {
    return Err(Error::plugin(
        "rivet.tool-shell requires process_spawn; the active profile denies it"
    ));
}
```

### 4.3 `Err` 와 `is_error` 를 구분한다

| 상황 | 반환 |
|---|---|
| 테스트가 실패했다 · 컴파일 에러 · 파일 없음 | `Ok(ToolResult::error(...))` — 모델이 대응해야 함 |
| 도구를 실행할 수 없다 · 권한 없음 · 내부 버그 | `Err(Error)` — 런타임이 처리 |

이 구분을 틀리면 모델이 고칠 수 있는 문제에서 Run이 죽거나, 고칠 수 없는 문제로 모델이
무한 재시도한다.

### 4.4 에러에 분류를 붙인다

```rust
Err(Error::transient(Capability::Model, "connection reset"))   // 재시도됨
Err(Error::invalid_argument("schema mismatch"))                // 재시도 안 됨
Err(Error::rate_limited(Some(7_000), "429"))                   // 7초 후 재시도
```

권한도 정확 일치가 아니라 순서를 존중해 확인한다.

```rust
// FsRead(Workspace) 를 가지고 있으면 아래는 true
ctx.permissions.allows(&Permission::FsRead(FsScope::Subtree("docs".into())))
```

분류를 안 붙이면 "영구 실패로 간주하라"는 뜻이 되고, 그것이 안전한 기본값이다.
`RetryPolicy`가 provider 메시지를 문자열 매칭하지 않게 하려는 것이 요점이다.

### 4.5 취소를 전파한다

```rust
tokio::select! {
    result = do_work() => result,
    () = ctx.host.cancelled() => Err(Error::cancelled("tool cancelled")),
}
```

취소를 무시하는 도구 하나가 Run 전체를 멈춘다.

### 4.5b 프로세스는 host를 통해 띄운다

```rust
// 이렇게 하지 않는다 — sandbox·timeout·출력 상한이 전부 무시된다
tokio::process::Command::new("cargo").arg("test").spawn()?;

// 이렇게 한다
let output = ctx.host.exec(ExecSpec::new("cargo", ["test".into()])).await?;
```

직접 spawn한 프로세스는 Policy가 고른 격리 밖에서 돌고, 취소 시 고아로 남는다.

### 4.6 `ToolContext`는 두 조각이다

```rust
pub struct ToolContext {
    pub data: ToolContextData,   // 직렬화 가능: ids, workspace, permissions, 한도
    pub host: Arc<dyn ToolHost>, // 살아있는 능력: progress / cancel / exec
}
```

**이 분리가 Phase 6의 전제다.** Tool이 별도 프로세스나 WASM에서 돌 때 `data`는 경계를
그대로 건너가고 `host` 메서드는 RPC가 된다. `data`에 직렬화 불가능한 것을 넣지 말고,
`host`가 주는 능력만 쓰면 Phase 6에서 코드를 바꾸지 않아도 된다.

### 4.7 Tool 이름은 provider 교집합을 따른다

`^[a-zA-Z0-9_-]{1,64}$`. 점·공백·유니코드는 어떤 provider에서 조용히 실패한다.

### 4.8 출력을 스스로 자른다

큰 출력을 그대로 반환하면 컨텍스트를 잡아먹는다. 잘랐다면 `Truncation`으로 신고한다.

```rust
Ok(ToolResult::ok(head).with_structured(json!({ "lines": total })))
```

---

## 5. 관찰과 개입

| 목적 | 계약 | 차단 | 허용 확대 | 비고 |
|---|---|---|---|---|
| 로그·지표 수집 | `EventSubscriber` | ✗ | ✗ | 느리면 이벤트를 잃음 |
| 제한 추가 | `Interceptor` | ✓ | **✗** | 타임아웃 있음 |
| 허가 규칙 | `Policy` | ✓ | ✗ | 순수 함수여야 함 |

구독자는 **차단할 수 없다.** 이것은 제약이 아니라 설계다 — 모든 구독자가 차단 가능하면
"무엇이 내 도구를 막았는가"에 답할 수 없다.

Interceptor는 `RestrictiveDecision`을 반환하며 여기에는 단독 `Allow` 변형이 **없다.**
결과는 Policy chain과 같은 fold에 합류하므로 단축되지 않는다. `priority()`는 사용자가 어느
이유를 먼저 보는지만 정하고 결과는 바꾸지 않는다.

`Modify`는 `Allow` + rewrite로 합류한다. 넓히는 rewrite를 쓴다고 해도 안전한데, 재작성된
호출은 스키마 검증과 정책 체인을 **처음부터 다시** 통과해야 하기 때문이다. rewrite가 호출의
id나 tool 이름을 바꾸면 그 자리에서 거부된다.

구독자 등록은 곧 전달 시작이다. `register_subscriber`가 이벤트 펌프까지 띄우므로 별도
attach 호출을 잊어 조용히 아무것도 못 받는 일이 없다. 언로드 시 런타임이 그 태스크를
중단한다.

```rust
#[async_trait]
impl EventSubscriber for MetricsPlugin {
    fn name(&self) -> &str { "metrics" }
    fn topics(&self) -> Vec<String> { vec!["tool.".into(), "agent.request.".into()] }
    async fn on_event(&self, e: &EventEnvelope) {
        // 빨리 반환한다. 느리면 SubscriberLagged 가 발행되고 이벤트를 잃는다.
        self.counter.record(e.topic());
    }
}
```

---

## 6. 테스트

```rust
#[tokio::test]
async fn tool_registers_and_unregisters_cleanly() {
    let bus = Arc::new(BroadcastBus::new());
    let registry = Registry::new(bus);
    let owner = Owner {
        plugin_id: PluginId::new("example.hello").unwrap(),
        instance_id: PluginInstanceId::new(),
    };

    HelloPlugin.load(ctx_for(&registry, &owner)).await.unwrap();
    assert!(registry.tool("hello").await.is_some());

    registry.unregister_all(owner.instance_id).await;
    assert!(registry.tool("hello").await.is_none());
}
```

최소 다음 다섯 가지를 테스트한다.

1. 정상 등록 · 해제
2. `load()` 중 실패 시 잔여물 없음
3. 권한이 거부됐을 때 명확히 실패
4. 취소가 5초 안에 반영
5. `Err` / `is_error` 구분이 의도대로

---

## 7. Lifecycle

```text
DISCOVERED → VALIDATED → LOADED → ACTIVE → UNLOADING → UNLOADED
                  │                                  ↑
                  └── ABI/권한 검사 실패 → FAILED      │
                                                     │
              load() 중 실패 → unregister_all() ──────┘
```

`VALIDATED` 단계에서 ABI와 권한이 검사되므로, 호환되지 않는 plugin은 **등록을 시도하기
전에** 걸러진다.

---

## 8. In-process → 외부 Plugin

| Phase | 형태 | 신뢰 | 시점 |
|---|---|---|---|
| 1 | Rust trait | Trusted | MVP |
| 2 | Process + JSON-RPC | Semi-trusted | Phase 6 |
| 3 | WASM | Capability-based | 이후 |

지금 in-process plugin을 쓰더라도 위 규칙을 지키면 Phase 6 전환 시 **plugin 코드를
바꾸지 않아도** 된다. 특히:

- 런타임 핸들을 붙잡지 않는다 (직렬화 불가)
- `PluginContext`가 준 것만 쓴다
- 에러를 분류해서 반환한다 (경계를 넘어 직렬화됨)
