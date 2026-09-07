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
| `Workflow` | `Workflow` | Job 그래프 실행 정책 |
| `Scheduler` | `Scheduler` | READY job → Agent Run |
| `Evaluator` | `Evaluator` | 완료된 Run 채점 |
| `EventSubscriber` | `EventSubscriber` | 관찰 (차단 불가) |
| `Command` | — | CLI 하위 명령 (Phase 3에는 **없다** — 아래 각주) |

> `Command`에 각주: `docs/plan.md`의 Phase 3 작업 항목(3.1–3.5)에 CLI 하위 명령이 없다.
> 작업 항목의 출처는 plan.md이므로 Phase 3은 이것을 만들지 않았다. 이 표와 plan.md 중
> 어느 쪽이 틀렸는지는 그 명령이 실제로 필요해지는 Phase에서 정한다.

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

이 파일은 crate 루트에 두고 코드에 문자열로 박아 넣는다.

```rust
pub const MANIFEST_TOML: &str = include_str!("../rivet-plugin.toml");
```

**매니페스트는 파일 하나뿐이다.** Rust 리터럴로 따로 만들지 않으므로 `rivet plugin show`가
보여주는 것과 plugin의 `manifest()`가 돌려주는 것이 어긋날 수 없다.

`abi_version`은 **등록 전에** 검사된다. 호환되지 않으면 plugin이 아무것도 등록하지
못한 채 거부된다 — `load()`가 호출조차 되지 않는다.

major `0`은 Cargo와 같이 **모든 minor 변경을 breaking으로** 취급한다.
`0.1` 호스트는 `0.1` plugin만 받는다.

### 2.1 파싱 규칙

**모르는 키는 에러다.** `[plugin]` 안에서도, `[[permissions]]` 항목 안에서도, 최상위
테이블에서도. 오타가 조용히 "없는 권한"이 되는 쪽이 훨씬 나쁘다.

`capabilities`는 비어 있을 수 없고, `abi_version`은 정확히 `major.minor`여야 한다
(`"0.1.2"`는 에러다 — 조용히 버려지는 것보다 낫다).

### 2.2 `scope` 문법

| `permission` | `scope` | 뜻 |
|---|---|---|
| `fs_read` / `fs_write` | `"workspace"` / `"anywhere"` | 워크스페이스 / 호스트 전체 |
| `fs_read` / `fs_write` | `{ subtree = "docs/api" }` | 그 하위 트리만 |
| `network_http` | 없음 | 모든 호스트 |
| `network_http` | `["api.openai.com"]` | 허용 목록 (빈 배열은 에러) |
| `secrets_read` | `["DEEPSEEK_API_KEY"]` | 필수, 비어 있을 수 없음 |
| 나머지 전부 | 없음 | `scope`를 적으면 에러 |

`{ subtree = "../../../etc" }`처럼 워크스페이스를 벗어나는 하위 트리는 **파싱 시점에**
거부된다. 나중에 meet에서 조용히 사라지게 두면, 매니페스트가 잘못됐다는 사실 대신 권한이
0개인 채로 로드된 plugin이 남는다.

### 2.3 선언하지 않은 슬롯에는 등록할 수 없다

`capabilities`에 없는 슬롯에 `register_*`를 부르면 로더가 그 호출을 거부하고, 그 plugin의
load 전체가 롤백된다. 선언한 슬롯이라도 **`load` 밖에서는** 등록할 수 없다(§4.1). `Interceptor`에는 대응하는 `CapabilityKind`가 없으므로
`capabilities = ["policy"]`로 선언한다 — 둘 다 "이 tool call이 진행돼도 되는가"에
답하는 자리다.

### 2.4 in-process plugin은 호스트 카탈로그에도 등록한다

in-process plugin은 링크된다. 매니페스트를 쓰는 것만으로는 아무 일도 일어나지 않고,
호스트의 카탈로그(`crates/rivet-cli/src/catalog.rs`)에 한 줄이 필요하다.

```rust
PluginSource::builtin("acme-tool-lint", acme_tool_lint::MANIFEST_TOML,
                      |m| Arc::new(acme_tool_lint::ToolLintPlugin::new(m))),
```

`rivet plugin new <id>`가 crate를 만들고 이 줄을 그대로 출력한다. 별도 프로세스 로딩은
Phase 6다.

### `events_subscribe`의 scope

토픽 **접두사** 목록이고, 생략하면 전체다.

```toml
[[permissions]]
permission = "events_subscribe"
scope      = ["tool.", "agent.run."]   # 생략 = 모든 토픽
```

호스트 allowlist와 달리 두 scope의 meet은 문자열 교집합이 **아니다.** 접두사 둘이 공통
토픽을 가지려면 한쪽이 다른 쪽의 접두사여야 하고, 그때 **긴 쪽**이 답이다 — 프로파일이
`tool.`을 주고 매니페스트가 `tool.execute.`를 요청하면 결과는 `tool.execute.`이지
`tool.`이 아니다. `tool.`과 `run.`은 공통 토픽이 없으므로 권한이 0개가 된다.

scope 목록은 **집합**이다. `TopicScope`(토픽)와 `StringSet`(호스트·시크릿 키)이 생성
시점에 정렬·중복 제거하고, 토픽은 다른 항목이 이미 덮는 접두사까지 흡수한다. 그래서
`["tool.", "tool.execute."]`와 `["tool."]`은 **같은 값**이고, 적은 순서가 판정을 바꾸지
않는다. 빈 목록과 빈 접두사는 생성자가 거부한다 — 빈 접두사는 모든 토픽에 걸려서 좁아
보이면서 전체를 주고, 빈 목록은 meet에는 "겹침 없음"이고 `topic_matches`에는 "전부"라
양끝이 반대로 읽힌다. `Deserialize`도 같은 생성자를 지난다.

`readonly` · `reviewer` · `production`은 `agent.text`를 주지 않는다
([`security.md` §8](./security.md)). `events_subscribe(["agent.text."])`를 선언한 plugin은
그 프로파일에서 교집합이 비므로 아래 관용구 (1)이 발동한다.

**Phase 3부터 이 scope는 강제된다.** 규칙 셋:

1. **빈 `topics()`는 "전부"가 아니라 grant의 목록이다.** 선호를 적지 않은 구독자는
   프로파일이 주는 만큼을 받는다. (`an_empty_topics_list_becomes_the_grant_not_everything`)
2. **`events_subscribe`가 없으면 구독자를 등록할 수 없다.** `register_subscriber`가 `Err`를
   낸다. 에러 문장은 둘로 갈린다 — 매니페스트가 요청한 적이 없거나, 프로파일이 깎았거나.
   운영자가 할 일이 다르기 때문이다(매니페스트를 고쳐라 / `--profile`을 바꿔라).
   (`a_manifest_that_never_asked_and_a_profile_that_removed_it_say_different_things`)
3. **grant와 겹치지 않는 `topics()`는 `Err`다.** 조용히 빈 목록으로 넘기면 (1) 때문에
   "전부"가 되어 정반대로 실패한다.
   (`a_subscriber_whose_topics_fall_outside_its_grant_is_refused`)

배달되는 목록은 `grant ⊓ topics()`이고, 그 meet은 `manifest ∩ profile`이 쓰는 것과 **같은
함수**다(`Permission::meet`). 판정 지점이 둘로 갈라지지 않도록 새 헬퍼를 쓰지 않았다.
등록 시점에 고정되므로 plugin이 나중에 자기 `topics()`를 넓혀도 배달은 안 넓어진다.

**부분적으로 좁혀지는 경우는 거절이 아니다.** 접두사의 meet은 합집합이므로
`["tool.", "agent.text."]`를 `readonly`에 태우면 `tool.`만 살아남고 meet은 비지 않는다 —
호스트는 통과시키고 `tracing::debug!`로 좁혔다는 사실과 양쪽 목록만 남긴다. 운영자가
*적어 넣은* 것이 사라지는 것을 거절로 만들지 말지는 plugin 자신의 몫이다. `rivet.telemetry-log`가
그 관용구를 쓴다(§4.2의 관용구 (1)).

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

/// The loader parses `rivet-plugin.toml` and hands the result to the constructor, so the
/// plugin never builds a manifest of its own.
#[derive(Debug)]
pub struct HelloPlugin {
    manifest: PluginManifest,
}

impl HelloPlugin {
    pub fn new(manifest: PluginManifest) -> Self {
        Self { manifest }
    }
}

#[async_trait]
impl Plugin for HelloPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
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
**인스턴스마다 다른 토큰이다** — 옆 plugin이 언로드돼도 내 백그라운드 작업은 멈추지
않는다. 이벤트 구독 태스크는 런타임이 소유자별로 추적해 언로드 시 abort하지만, plugin이
직접 띄운 태스크는 plugin 책임이다.

**등록은 `load` 안에서만 할 수 있다.** 로더는 `load`가 반환된 직후 등록 창구를 닫고,
`unload`를 부르기 전에 한 번 더 닫는다. 닫힌 뒤의 `register_*`는 plugin 이름을 담은
`Err`로 실패하고 `warn` 로그를 남긴다 — `load`가 띄운 태스크가 나중에 부르는 것도,
`unload` 안에서 부르는 것도 같다. 연결이 선 다음에 등록하고 싶다면 그 연결을 `load`
안에서 기다려야 한다.

이유는 회계다. 실패한 load의 롤백은 `unregister_all(instance_id)` **한 번**이고, 그
뒤에 도착한 등록은 이미 죽은 인스턴스 id가 소유하게 된다 — 어떤 `unload`도 그것을
언급하지 않고, `rivet doctor`와 `rivet plugin list`는 `record.registered`를 읽으므로 그
존재를 볼 수도 없다. 프로세스 안의 무엇으로도 지울 수 없는 capability가 남는다는 뜻이다.
`unload` 쪽도 같은 사고다: 로더는 `unregister_all`을 먼저 돌리고 그 다음에
`Plugin::unload`를 부르므로, teardown 중의 등록은 정리를 지나쳐 살아남아 **자기 자신의
재로드를 영구히 막는다.**

**`load`가 반환되는 것과 *동시에* 도착한 등록은 동전 던지기다.** 창구는 `load`가 반환된
직후에 닫히므로, 그 직전에 띄운 태스크의 등록은 봉인을 앞지를 수도, 봉인에 걸릴 수도
있다 — 리뷰 3라운드가 같은 plugin으로 25번 돌려 64건 중 13~64건이 통과하는 것을 측정했다.
어느 쪽이 되든 **회계는 어긋나지 않는다**: 통과하면 레지스트리와 `record.registered`에
함께 들어가고, 걸리면 어느 쪽에도 안 들어간다. 어긋나는 것은 `PluginHandle`이다. 핸들은
`load`가 반환될 때 고정되므로, 창구를 통과한 등록 하나가 핸들에 없으면
`claim_matches_reality()`가 false가 되고 `rivet doctor`가 "reported" 경고를 낸다. **같은
설정으로 `rivet doctor`를 두 번 돌렸는데 한 번만 경고가 뜬다면 이 경우다** — 무작위가
아니라 태스크에서 등록하는 plugin이 있다는 뜻이고, 고칠 곳은 타이밍이 아니라 그 태스크다.

**언로드는 협조적인 구독자에 대해서만 확실하다.** 레지스트리는 언로드 시 그 plugin의 구독
펌프를 `JoinHandle::abort`로 끊는데, abort는 다음 await 지점에서 효력이 있다. `on_event`
안에서 동기적으로 블로킹하면(막힌 파이프로의 로그 쓰기 같은) 그 지점이 없다. 경계는 좁다 —
루프는 안 멈추고(발행은 동기이며 배달은 별도 태스크다), 멈추는 것은 그 구독자의 펌프 하나이며,
레지스트리 표에서는 이미 사라졌으므로 `unload`는 반환하고 이름은 재로드에 쓸 수 있다. 회계는
어긋나지 않고 태스크 하나가 프로세스 끝까지 샌다. `on_event`는 **즉시 반환해야 한다**는
계약이 이것 때문에 있다.

### 4.2 권한은 넓힐 수 없다

```text
effective = manifest.permissions ∩ profile.permissions ∩ session overrides
```

`ctx.permissions`가 실제로 받은 것이다. 매니페스트에 적었다고 받은 게 아니다.

권한이 깎였을 때 어떻게 할지는 **두 가지 관용구가 있고 둘 다 옳다.** 어느 쪽인지는
plugin이 정한다.

```rust
// (1) 크게 실패한다 -- 그 권한 없이는 이 plugin이 할 일이 없을 때.
//     `rivet.model-openai`가 network_http에 대해 실제로 하는 것이 이쪽이다.
if !ctx.permissions.granted().iter().any(|p| matches!(p, Permission::NetworkHttp(_))) {
    return Err(Error::plugin(
        "`rivet.model-openai` needs `network_http`, and the active profile grants none"
    ));
}

// (2) 줄여서 등록한다 -- 남은 권한으로 할 수 있는 일이 아직 있을 때.
//     `rivet.tool-filesystem`이 readonly에서 write_file만 빼고 등록하는 것이 이쪽이다.
if ctx.permissions.allows(&Permission::FsWrite(FsScope::Workspace)) {
    ctx.registry.register_tool(Arc::new(WriteFile)).await?;
}
```

> **⚠ 어느 프로파일도 주지 않는 permission이 셋 있다** — `process_spawn`, `secrets_read`,
> `job_manage`. `Profile::permissions()`가 주는 것은 `fs_read(workspace)` ·
> `session_read` · `session_write` · `events_publish` · `network_http` ·
> `events_subscribe`, 그리고 쓰기 가능한 프로파일의 `fs_write(workspace)`뿐이다.
>
> Phase 2부터 교집합이 실제로 계산되므로, 이 셋 중 하나를 매니페스트에 적은 plugin은
> **모든 프로파일에서** 그 권한이 0개가 된다. 그 위에 관용구 (1)을 얹으면 그 plugin은
> 어디서도 로드되지 않는다 — `developer`에서도. (이 문단의 예시가 원래
> `rivet.tool-shell` + `process_spawn`이었던 이유이고, 그래서 실제로 동작하는
> `rivet.model-openai`로 바꿨다.) 매니페스트 파서가 `secrets_read`에 비어 있지 않은 키
> 목록을 요구하는 것은 **어휘가** 완성돼 있다는 뜻이지 프로파일이 그것을 준다는 뜻이
> 아니다.
>
> 지금 그런 슬롯이 필요하면 관용구 (2)로 축소 등록하는 수밖에 없다. 프로파일이 그
> 권한을 주도록 바꾸는 것은 정리가 아니라 **보안 결정**이라 열어 두었다 —
> [`security.md`](./security.md) §8 표의 각주와 [`architecture.md`](./architecture.md)
> §11-10.

**`fs_read`의 scope는 Phase 2에서 아무것도 좁히지 않는다.** `tool-filesystem`은
`write_file`만 grant로 게이트하고 `read_file`·`list_dir`·`search`는 무조건 등록한다. 이
읽기 도구들을 워크스페이스 안에 가두는 것은 grant가 아니라 `Workspace::resolve`와
런타임의 fsguard다. 그래서 `fs_read({ subtree = "docs" })`를 선언한 매니페스트도
워크스페이스 전체를 읽는 `read_file`을 받고, `rivet plugin show`는 그것을 `granted`로
출력한다. 즉 **읽기 scope는 지금 선언이지 강제가 아니다.** 도구별 경로 범위를 실제로
좁히는 것은 Phase 4의 sandbox이고, 파서가 탈출 서브트리를 지금 거부하는 것은 그 강제가
붙는 시점에 어휘가 이미 정확하도록 하기 위해서다. 쓰기 쪽(`fs_write`)은 DoD 3이며 지금도
진짜다.

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

    let manifest = rivet_plugin::parse(MANIFEST_TOML).unwrap();
    let plugin = HelloPlugin::new(manifest);
    plugin.load(ctx_for(&registry, &owner)).await.unwrap();
    assert!(registry.tool("hello").await.is_some());

    registry.unregister_all(owner.instance_id).await;
    assert!(registry.tool("hello").await.is_none());
}
```

최소 다음 여섯 가지를 테스트한다.

1. 정상 등록 · 해제
2. `load()` 중 실패 시 잔여물 없음
3. 권한이 거부됐을 때 명확히 실패
4. 취소가 5초 안에 반영
5. `Err` / `is_error` 구분이 의도대로
6. `rivet-plugin.toml`이 파싱되고, `version`이 crate 버전과 같고, `capabilities`가
   실제로 등록하는 슬롯을 전부 포함한다 — 로더가 load 시점에 잡아 주지만, 이 테스트는
   작성자가 아직 고칠 수 있는 빌드 시점에 잡는다

```rust
#[test]
fn the_manifest_matches_the_crate() {
    let manifest = rivet_plugin::parse(MANIFEST_TOML).unwrap();
    assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.capabilities, [CapabilityKind::Tool]);
}
```

---

## 7. Lifecycle

```text
DISCOVERED → VALIDATED → LOADED → ACTIVE → UNLOADING → UNLOADED
                  │                                  ↑
                  └── ABI/권한 검사 실패 → FAILED      │
                                                     │
              load() 중 실패 → unregister_all() ──────┘
```

`VALIDATED` 단계에서 ABI가 검사되고 `manifest ∩ profile`이 계산된다. 아무것도 생성되지
않으므로, 호환되지 않는 plugin은 **등록을 시도하기 전에** 걸러진다. 권한이 좁혀진 것은
실패가 아니다 — 그걸 어떻게 할지는 plugin이 `load`에서 정한다(§4.2).

`LOADED`에서 `ACTIVE`로는 **배치 전체가 성공했을 때만** 올라간다. 하나라도 실패하면
성공한 것들은 `LOADED`에 남고, 호스트가 곧 그것들을 정리한다. 즉 `LOADED`에 남아 있는
기록은 "곧 내려갈 것"이라는 뜻이다.

`UNLOADED`가 된 plugin은 **같은 이름으로 다시 로드할 수 있다.** 새 `PluginInstanceId`가
발급되므로 되살아난 인스턴스가 아니라 새 인스턴스다. `FAILED`는 종점이다 — 그 프로세스
안에서 재시도되지 않고, 이유는 `rivet plugin list`를 위해 보존된다.

`load()`가 **패닉**해도 `Err`와 같은 길을 간다: 등록이 되돌려지고, 그 인스턴스의
취소 토큰이 취소되고, 기록은 `FAILED`가 된다. (plugin이 직접 띄운 태스크 안의 패닉은
잡히지 않는다.)

**등록 창구는 `load`와 함께 닫힌다.** `load`가 반환되면 — 성공이든 실패든 — 로더가 그
plugin의 guard를 봉인하고, `Plugin::unload`를 부르기 전에 다시 봉인한다. 그래서 실패한
load가 남긴 태스크의 뒤늦은 등록도, `unload` 안의 등록도 거부된다. 롤백이 한 시점의
청소가 아니라 봉인이라는 뜻이다(§4.1).

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
