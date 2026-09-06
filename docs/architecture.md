# Rivet Architecture

> 상태: Draft v0.4 (설계 리뷰 2회전 반영) · 대상: MVP ~ MVP+
> 한 줄 정의: **The model thinks. The runtime acts. Plugins define behavior.**

이 문서는 Rivet의 구조적 결정과 **그 결정을 내린 이유**를 기록한다.
"무엇을 만드는가"는 [`plan.md`](./plan.md)에, 개별 계약의 세부는
[`plugin.md`](./plugin.md) · [`events.md`](./events.md) · [`job.md`](./job.md) ·
[`security.md`](./security.md)에 있다.

---

## 1. 문제 정의

기존 코딩 에이전트 대부분은 **하나의 통합된 프로그램**이다. Shell 실행, Git 조작,
권한 판단, 세션 저장, UI 렌더링이 하나의 루프 안에 얽혀 있다. 이 구조는 처음 3개월은
빠르지만 그 뒤로는 다음이 전부 어려워진다.

- 모델 교체 — 프롬프트 형식이 루프에 하드코딩되어 있다
- 권한 정책 변경 — 각 도구가 자기 안전성을 스스로 판단한다
- 감사 — 무슨 일이 있었는지 로그로 재구성할 수 없다
- 재개 — 중단된 작업을 이어받을 상태 표현이 없다
- 확장 — 기능 추가가 곧 루프 수정이다

Rivet의 목표는 에이전트를 하나 더 만드는 것이 아니라, **에이전트가 사용하는 능력
(capability)의 계약을 안정화하고 그 계약을 Plugin으로 확장하는 런타임**을 만드는 것이다.

계약이 안정되면 같은 런타임 위에서 Coding Agent, Review Agent, DevOps Agent,
CI Agent, Job Orchestrator를 조합할 수 있다.

---

## 2. 시스템 개요

```text
┌──────────────────────────────────────────────────────────────┐
│  Clients        CLI · TUI · JSONL · RPC · Daemon             │
│                 (전부 Event Stream 소비자. 런타임 내부 접근 없음) │
└───────────────────────────┬──────────────────────────────────┘
                            │
┌───────────────────────────▼──────────────────────────────────┐
│  Job Runtime       Graph · State Machine · Review · Scheduler │
│                    "무엇을 목표로 하는가"                        │
└───────────────────────────┬──────────────────────────────────┘
                            │  Job → Agent Run (1:N)
┌───────────────────────────▼──────────────────────────────────┐
│  Agent Runtime     Agent Loop · Context Assembly · Session    │
│                    "지금 무엇을 실행하는가"                      │
└───┬───────────────────┬───────────────────┬──────────────────┘
    │                   │                   │
┌───▼─────┐       ┌─────▼─────┐       ┌─────▼──────┐
│  Model  │       │   Tools   │       │   Policy   │
└─────────┘       └─────┬─────┘       └─────┬──────┘
                        │                   │
                        └─────────┬─────────┘
                            ┌─────▼──────┐
                            │  Sandbox   │
                            └─────┬──────┘
                            ┌─────▼──────┐
                            │ Execution  │
                            └────────────┘

════════════════════ Event Bus (관찰) ════════════════════
        │            │            │            │
    Session      Telemetry     Memory      Evaluation
   (durable)    (lossy)       (plugin)     (plugin)
```

수직 방향은 **제어 흐름**, 수평 Event Bus는 **관찰 흐름**이다. 이 둘을 분리한 것이
설계의 중심이다 (§5).

---

## 3. 설계 원칙

### 3.1 Core는 구현이 아니라 계약을 소유한다

`rivet-core`에는 I/O가 하나도 없다. HTTP 클라이언트도, `std::process`도,
파일시스템 접근도 없다. Core가 정의하는 것은 다음뿐이다.

| 종류 | 예 |
|---|---|
| Capability trait | `Model` `Tool` `Policy` `Sandbox` `ContextProvider` `Workflow` `Scheduler` `Memory` `Evaluator` |
| Domain type | `Job` `SessionEvent` `ToolSpec` `PermissionSet` `Workspace` |
| Event 정의 | `AgentEvent` `ToolEvent` `JobEvent` `PluginEvent` `RuntimeEvent` |
| Error / ID | `Error` `ErrorKind` `SessionId` `JobId` `PluginId` |

> **판별 기준**: `rivet-core`에 `reqwest`나 `tokio::process`를 추가하고 싶어졌다면,
> 그 추상화는 잘못된 위치에 있다.

### 3.2 Agent Loop는 작게 유지한다

Agent Loop의 책임은 정확히 다섯 가지다.

```text
Context 조립 → Model 호출 → 응답 분기 → Tool 위임 → Session 기록
```

Loop **안에** 넣지 않는 것: Git/Shell/Web 구현, 권한 판단, 저장소 구현, UI 로직,
Job 스케줄링, Retry 분기.

### 3.3 확장은 Event와 Capability로만 한다

Plugin은 런타임 내부 상태를 직접 만지지 않는다. 개입 경로는 정확히 세 개뿐이다.

| 목적 | 수단 | 차단 | 허용 확대 |
|---|---|---|---|
| 관찰 | `EventSubscriber` | ✗ | ✗ |
| 제한 추가 | `Interceptor` | ✓ | **✗ (타입으로 금지)** |
| 허가 판단 | `Policy` | ✓ | ✗ |

"내 tool call을 무엇이 막을 수 있는가"가 **열거 가능한 목록**이어야 하기 때문에
관찰과 차단을 분리했다 (§5.3).

`Interceptor`는 `RestrictiveDecision`을 반환한다 — `Allow` 변형이 아예 **존재하지
않는다.** 그리고 그 결과는 단축(short-circuit)이 아니라 Policy chain과 같은
`combine()` fold에 합류한다. 어느 확장 지점도 혼자서 결과를 정할 수 없다.

### 3.4 Tool과 Policy를 분리한다

```text
Model → Tool Call → Interceptor → Policy → Approval → Sandbox → Execute
```

Tool은 자신의 권한 정책을 결정하지 않는다. `ShellTool`은 `rm -rf`가 위험한지 모르며,
알 필요도 없다. 이것이 `readonly` 프로파일을 **실제로** 동작하게 만든다.

### 3.5 Agent와 Job을 분리한다

- **Agent Run** = 지금 진행 중인 한 번의 실행 시도
- **Job** = 목표와 상태를 소유하는 지속 단위

```text
Job ──┬── Agent Run #1 (구현)   → Session A
      ├── Review Run  #1 (검토) → Session B
      └── Agent Run #2 (수정)   → Session C
```

Job이 Run보다 오래 산다. 그래서 Run이 죽어도, 리뷰에서 반려돼도, 모델을 바꿔도
Job은 이어진다.

---

## 4. 핵심 계약

### 4.1 Model — 스트리밍 단일 경로

```rust
#[async_trait]
pub trait Model: Send + Sync + Debug {
    fn id(&self) -> &ModelId;
    fn capabilities(&self) -> ModelCapabilities;
    async fn stream(&self, request: ModelRequest) -> Result<ModelStream>;
    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64>;
}
```

**결정: 스트리밍만 지원한다.** 비스트리밍 provider는 어댑터가 청크 하나를 내고 종료하는
방식으로 감싼다. 반대로 하면(블로킹 API 위에 스트리밍을 흉내) 모든 UI가 두 경로를
구현해야 한다.

**결정: 모델은 아무것도 실행하지 않는다.** `ToolCall`을 **데이터로** 반환할 뿐이다.
실행은 런타임의 몫이고, 그 사이에 Policy가 있다.

`ModelStream`은 정확히 하나의 `StreamEvent::Done`으로 끝난다. 런타임이 turn 종료를
여기에 의존하므로 어댑터는 이를 보장해야 한다.

### 4.2 Tool — 순수한 능력

```rust
#[async_trait]
pub trait Tool: Send + Sync + Debug {
    fn spec(&self) -> ToolSpec;
    async fn execute(&self, ctx: ToolContext, input: Value) -> Result<ToolResult>;
}
```

`ToolContext`는 두 조각으로 나뉜다.

```rust
pub struct ToolContext {
    pub data: ToolContextData,   // 직렬화 가능한 순수 데이터
    pub host: Arc<dyn ToolHost>, // 살아있는 능력 (progress / cancel / exec)
}
```

**이 분리가 Phase 6의 전제다.** Tool이 별도 프로세스나 WASM에서 돌 때 `ToolContextData`는
경계를 그대로 건너가고, `ToolHost`의 메서드는 RPC 호출이 된다. 지금 나누지 않으면
"Phase 6에서 plugin 코드를 바꾸지 않아도 된다"는 약속을 지킬 수 없다 — `Arc<dyn EventBus>`나
`CancellationToken`은 프로세스 경계를 넘지 못한다.

`ToolContext`에 **없는 것**도 중요하다: `Runtime` 핸들, `ToolRegistry`, `SessionStore`가
없다. 런타임을 다시 호출할 수 있는 Tool은 Agent Loop에 재진입하거나, Policy를 우회하거나,
디스패처를 교착시킬 수 있다.

또한 Tool은 **프로세스를 직접 spawn하지 않는다.** `ctx.host.exec()`를 거쳐야 sandbox와
timeout과 출력 상한이 실제로 적용된다.

**`Err` vs `is_error`의 구분**은 반복해서 틀리는 지점이라 계약에 명시했다.

| 상황 | 반환 | 이유 |
|---|---|---|
| 테스트 실패, 컴파일 에러 | `Ok(ToolResult { is_error: true })` | 모델이 읽고 대응해야 한다 |
| 도구 자체가 실행 불가 | `Err(Error)` | 모델이 도울 수 없다 |

`ToolAnnotations`(`read_only`, `destructive`, …)는 **작성자의 주장일 뿐 강제가 아니다.**
악의적이거나 버그 있는 plugin은 거짓말할 수 있다. Policy는 이것만 믿고 허용하면 안 된다.
그럼에도 존재하는 이유는, 기본 정책이 "세상 모든 도구 목록"을 손으로 관리하지 않고도
엄격할 수 있게 하기 위해서다.

### 4.3 Policy — 결정론적 합성

```rust
pub struct PolicyDecision {
    pub outcome: Outcome,                 // Allow | RequireApproval | Deny
    pub rewrite: Option<ToolCall>,        // 좁히는 방향의 재작성
    pub constraints: ExecutionConstraints, // timeout · sandbox · 출력 상한 · 권한
}
```

**세 가지가 별도 필드인 이유**: 서로 독립이기 때문이다. 처음에는 `Modify`를 `Allow`와
`RequireApproval` 사이 severity를 갖는 enum 변형으로 두었는데, 두 가지 버그가 나왔다.

- 명령을 `--dry-run`으로 좁힌 정책의 rewrite가, **다른** 정책이 승인을 요구하는 순간
  조용히 버려지고 원본이 승인됐다.
- 무제약 `Allow` 하나가 다른 정책의 `sandbox = docker` 요구를 지웠다.

분리하면 rewrite와 constraint가 outcome과 무관하게 살아남는다.

**결정: 가장 제한적인 결정이 이긴다 (most-restrictive-wins).**

```text
outcome severity:  Allow(0) < RequireApproval(1) < Deny(2)
constraints:       축별로 더 엄격한 값 (min timeout · sandbox Some 우선 · 권한 교집합)
rewrite:           하나면 살아남고, 서로 다른 둘이면 Deny
```

서로 다른 두 rewrite를 합성하는 것은 런타임이 추측으로 할 수 있는 일이 아니고, 하나를
임의로 고르면 다른 하나의 좁힘이 조용히 사라진다. 그래서 **거부**한다.

이 규칙을 각 host가 아니라 Core에 박아 넣었다. **등록 순서에 따라 달라지는 보안 결정은
보안 결정이 아니기 때문이다.** 동점이면 왼쪽을 유지하므로 fold가 결정론적이다.

`rewrite`는 **좁히는 방향으로만** 허용한다(`--dry-run` 추가, 경로 제한). 두 가지가 이를
강제한다.

1. `rewrite_is_wellformed()`가 호출의 **정체성**(id와 tool 이름)이 유지되는지 검사한다.
   둘 중 하나를 바꾼 "rewrite"는 이 호출의 승인을 걸치고 나타난 다른 호출이다.
2. 런타임이 재작성된 호출에 대해 **파이프라인 전체를 다시 돌린다** — 스키마 검증과 정책
   체인 전부를, 작은 고정 깊이까지. 인자를 넓히는 rewrite라도 실행 전에 다시 판정되므로,
   거부된 호출을 허용된 호출로 세탁할 수 없다.

Interceptor의 결과도 이 fold에 합류한다. `RestrictiveDecision`에는 `Allow`가 없으므로
interceptor는 결과를 **더 엄격하게만** 만들 수 있다. Interceptor가 `Some`을 반환해도
Policy chain은 계속 평가되며, 더 제한적인 쪽이 이긴다. 등록 순서나 이름 알파벳순이
결과를 바꿀 수 없다.

**`unattended` 플래그**: CI·headless·daemon에서는 사람이 답할 수 없다. 이때
`RequireApproval`은 런타임이 `Deny`로 변환한다. 매달려 있는 것보다 예측 가능하게
거부되는 편이 낫다.

Policy는 `PolicyRequest`의 **순수 함수**여야 한다. 전역 상태를 읽거나 파일시스템을
조회하면 안 된다. 그래야 감사 시점에 세션 로그만으로 결정을 재현할 수 있다.

### 4.4 Sandbox — 정직한 보장

Policy가 "해도 되는가"라면 Sandbox는 "어떤 격리 아래에서"다. 같은 `git push`가
노트북에서는 허용·비격리, CI에서는 허용·컨테이너일 수 있고, 둘 중 하나가 다른 하나를
조용히 바꾸면 안 된다.

```rust
pub struct SandboxGuarantees {
    filesystem_isolation: bool,
    network_isolation: bool,
    process_isolation: bool,
    copies_workspace: bool,
}
```

**결정: 각 provider는 실제로 무엇을 강제하는지 스스로 보고한다.** `sandbox-local`은
`network_isolation: false`를 반환하고, UI는 그대로 표시한다. 운영자가 자기가 무엇을
받고 있는지 오해하는 상황을 만들지 않는다.

`ExecSpec`의 두 가지 기본값도 의도적이다.

- **셸 문자열을 받지 않는다.** 셸이 필요하면 호출자가 `program = "sh", args = ["-c", …]`로
  명시한다. 주입 표면이 문자열 연결 속에 숨지 않고 세션 로그에 드러난다.
- **환경변수는 빈 상태에서 시작한다.** 상속하지 않는다. `AWS_SECRET_ACCESS_KEY`가
  샌드박스로 새려면 누군가 그것을 명시적으로 적어야 한다.

### 4.5 Context — 예산 인식 조립

순진한 설계("provider가 준 걸 다 이어붙인다")는 컨텍스트 윈도를 넘는 순간 무너진다.
그래서 계약이 처음부터 예산을 안다.

```rust
pub struct ContextItem {
    slot: ContextSlot,      // SystemPrompt → Environment → Job → Memory
                            // → Skills → RuntimeState → History
    priority: Priority,     // Optional < Normal < Important < Required
    key: String,            // 안정적 키: dedupe + prompt cache 적중
    content: String,
    estimated_tokens: Option<u32>,
}
```

`fit_to_budget()`은 **항목 중간을 자르지 않고 낮은 우선순위부터 통째로 버린다.**
동점은 slot 순서 → 선언 순서로 깬다. 결정론적이어야 prompt caching과 replay가 의미를
가진다.

두 가지를 명시적으로 보장한다.

1. **우선순위 역전 금지.** 한 항목이 예산에 안 들어가면 그보다 **낮은** 우선순위는
   전부 버린다. 단순 greedy fill은 400토큰짜리 `Important`를 건너뛰고 40토큰짜리
   `Optional`을 넣는데, 그것은 우선순위의 정반대다.
2. **key 기준 중복 제거.** provider는 매 turn 재실행되며 두 provider가 같은
   `git.status`를 낼 수 있다. 첫 항목이 이기고, 그래야 turn 간 프롬프트 모양이 안정되어
   prompt caching이 계속 적중한다.

**토큰 추정은 바이트가 아니라 문자 기반이다.** 흔한 `bytes / 4` 규칙은 영어 기준이며
한국어·일본어·중국어에서 2~4배 **과소** 추정한다(한글 한 글자 = UTF-8 3바이트, 그러나
보통 1~2 토큰). 과소 추정은 위험한 방향이다 — 예산이 "들어간다"고 답한 뒤 컨텍스트
윈도가 넘친다. `estimate_tokens()`는 non-ASCII 문자에 더 큰 가중치를 준다.

`Model::count_tokens()`의 기본 구현은 system prompt, **tool schema**, 그리고 모든 메시지
블록(추론·tool 인자·tool 결과·이미지)을 전부 센다. 텍스트 블록만 세면 200KB짜리 tool
출력이 0 토큰으로 보고되어, 예산 축이 가장 필요한 tool 중심 실행에서 무의미해진다.

**`Required` 항목만으로 예산을 넘으면 조립은 실패한다.** 조용히 깨진 프롬프트를 보내는
것보다 크게 실패하는 편이 낫다. 버려진 항목은 `AssembledContext.dropped`로 보고되어
UI가 경고하고 evaluator가 품질과 상관관계를 볼 수 있다.

### 4.6 Session — 재생 가능한 사실의 로그

세션은 append-only 이벤트 로그이며 **단일 진실 원천**이다. 현재 상태는 replay로
재구성한다.

```text
Created → RunStarted → UserMessage → ModelRequested → AssistantMessage
        → ToolCalled → ToolCompleted | ToolBlocked → RunCompleted → …
        → Checkpoint → Closed
```

세 가지 결정:

1. **`ModelRequested`는 요청을 보내기 *전에* 기록한다.** 요청 도중 크래시하면 replay에서
   "완료 없는 요청"으로 드러난다. 사후에 기록하면 그 크래시는 흔적을 남기지 않는다.
2. **`ToolBlocked`도 durable fact다.** "모델이 요청했고 거부당했다"는 것이 감사에서
   정확히 필요한 사실이다. 또한 replay 시 모델에게 거부 사유가 담긴 tool result를
   만들어 준다 — 그러지 않으면 모델은 같은 호출을 무한 재시도한다.
3. **`ToolCalled`는 messages로 투영하지 않는다.** 호출 자체는 직전 `AssistantMessage`의
   `ContentBlock::ToolCall`에 이미 있다. 별도로 로깅하는 이유는 *디스패치가 시작된 시점*을
   표시하기 위해서다.
4. **승인도 durable fact다.** `ApprovalRequested` / `ApprovalResolved`가 로그에 남는다.
   "누가 언제 `rm -rf`를 승인했는가"는 감사가 정확히 묻는 질문이고, 버스는 lossy다.
   `ApprovedForSession`(세션 동안 기억)도 로그의 **projection**이므로 재개 후에도
   원본과 같게 동작한다. 메모리에만 두면 resume 시 사라진다.

### 중단된 tool call 복구

`ToolCalled`와 `ToolCompleted` 사이에서 프로세스가 죽으면, 로그에는 **아무도 답하지 않은
tool call을 든 assistant 메시지**가 남는다. 모든 주요 provider가 이런 메시지 배열을
400으로 거부하므로, 순진한 replay는 **재개 자체가 불가능한** 세션을 만든다.

그래서 `SessionState::replay()`는 로그 끝에 미해결 호출이 남아 있으면 각각에
`is_error: true`인 합성 결과를 만들어 붙인다. 모델은 도구가 중단됐다는 사실을 보고
재시도 여부를 스스로 판단한다 — 사람이 할 판단과 같다.

### 내구성

`SessionStore::append`는 **이벤트가 매체에 durable해진 뒤에** 반환해야 한다(`fsync` 또는
등가). 버퍼링 후 즉시 반환하는 구현은 로그 꼬리를 잃는데, 그 크래시가 바로 이 시스템이
존재하는 이유다. 배치는 허용하되, 배치가 durable해지기 전 반환은 허용하지 않는다.

`append(id, expect, event)`의 `Expect`가 동시 쓰기를 안전하게 만든다. `Option<u64>`가
아니라 enum인 이유는, 동시성 보호를 포기하는 것이 **작성자가 직접 타이핑한 단어**여야
하기 때문이다(`Expect::Any`).

---

## 5. 가장 중요한 구분: Bus Event vs Session Event

이것이 이 설계에서 가장 자주 잘못 구현되는 부분이다.

| | Session Event | Bus Event |
|---|---|---|
| 성격 | **durable fact** | **notification** |
| 저장 | 항상 | 안 함 |
| 유실 | 절대 불가 | 허용 (느린 구독자) |
| 순서 | 조밀한 `seq` 보장 | best-effort |
| 예 | `assistant.message` | `agent.text.delta` |
| 용도 | replay · resume · audit · fork | UI · telemetry · 관찰 |

`agent.text.delta`는 버스에만 흐르고 저장되지 않는다. 세션은 **조립된 메시지**를
저장한다. replay가 델타에서 메시지를 다시 유도할 필요가 없기 때문이다. 이 둘을 섞으면
결정론적으로 재생할 수 없는 로그가 된다.

### 5.1 발행은 절대 막히지 않는다

```rust
pub trait EventBus: Send + Sync + Debug {
    fn publish(&self, envelope: EventEnvelope);   // async 아님, Result 아님
}
```

느린 telemetry 구독자에 의해 멈출 수 있는 Agent Loop는 **언젠가 반드시 멈춘다.**
그래서 `publish`는 동기이고 실패하지 않는다.

### 5.2 유실은 조용하지 않다

용량을 넘긴 구독자는 이벤트를 잃는다. 그러나 그 사실은
`RuntimeEvent::SubscriberLagged { subscriber, dropped }`로 **보고된다.** 막지도 않고
조용히 버리지도 않는, 두 실패 모드 사이의 유일하게 정직한 선택지다.

### 5.3 Interceptor는 Subscriber와 다르다

| | `EventSubscriber` | `Interceptor` |
|---|---|---|
| 반환값 | 없음 | `Option<PolicyDecision>` |
| 실행 차단 | 불가 | 가능 |
| 개수 | 임의 | 열거·정렬·타임아웃됨 |

모든 구독자가 차단할 수 있다면 "무엇이 내 tool call을 막을 수 있는가"에 답할 수 없다.
Interceptor는 타임아웃을 받으며, 멈춘 interceptor는 `None`으로 처리되고 보고된다.

---

## 6. Tool 실행 파이프라인

순서는 고정이며 host가 바꿀 수 없다.

```text
      ToolCall (모델이 요청)
            │
    ┌───────▼────────┐
    │ 1. Resolve     │  등록 안 됨 → is_error 결과로 모델에 회신
    ├────────────────┤
    │ 2. Scope check │  agent.allows_tool()
    ├────────────────┤
    │ 3. Validate    │  JSON Schema. Policy가 항상 정형 입력을 보게 함
    ├────────────────┤
    │ 4. Intercept   │  Some(decision) → 단축, None → 다음
    ├────────────────┤
    │ 5. Policy      │  chain fold, most-restrictive-wins
    ├────────────────┤
    │ 6. Approval    │  RequireApproval && !unattended 일 때만
    ├────────────────┤
    │ 7. Sandbox     │  constraints.sandbox 지정 시 prepare
    ├────────────────┤
    │ 8. Execute     │  ctx.host 취소 준수 · host.exec 경유 필수
    ├────────────────┤
    │ 9. Truncate    │  max_output_bytes, 원본은 artifact로
    ├────────────────┤
    │10. Persist     │  ToolCompleted 또는 ToolBlocked
    └────────────────┘
```

**3번이 5번보다 앞인 이유**: Policy는 항상 스키마 검증을 통과한 입력을 본다. 반대라면
각 Policy가 방어적 파싱을 중복 구현해야 하고, 그 중 하나는 반드시 틀린다.

실패 시:

```text
Tool Error → ErrorKind 분류 → RetryPolicy → RetryAfter | Stop
```

재시도 여부를 provider 메시지 문자열 매칭으로 판단하지 않기 위해, 모든 에러가
`ErrorKind`를 **운반한다.** 분류를 붙이지 않은 plugin은 "영구 실패로 간주하라"고 말하는
셈이고, 그것이 안전한 기본값이다.

| `ErrorKind` | 재시도 | 사람 필요 |
|---|---|---|
| `Transient` `Timeout` `RateLimited` | ✓ | |
| `InvalidArgument` `NotFound` `Upstream` | | |
| `PolicyDenied` `ApprovalDenied` | | ✓ |
| `Cancelled` | | |

서버가 `retry_after_ms`를 주면 로컬 백오프 곡선보다 우선한다. 언제 다시 오라고 말해주는
upstream이 로컬 곡선보다 잘 안다.

---

## 7. Agent Loop

```text
START
  │
  ├──► 한도 확인 (turns · duration · tokens · context · 연속 도구 실패)
  │       └─ 초과 → StopReason::LimitReached { limit } 로 종료
  │
  ├──► Context 조립 (providers → fit_to_budget)
  │
  ├──► Session: ModelRequested 기록
  │
  ├──► model.stream()  ──► 델타를 Bus로 (저장하지 않음)
  │
  ├──► Session: AssistantMessage 기록 (조립된 메시지)
  │
  └──► stop_reason 분기
         ├─ EndTurn      → STOP
         ├─ MaxTokens    → 미완결 turn. 계속 여부는 정책 (§7.1)
         └─ ToolUse      → 각 call 을 파이프라인(§6)으로
                            → 결과 기록 → CONTINUE
```

### 7.1 한도는 다섯 축 모두를 막는다

```rust
pub struct RunLimits {
    max_turns: u32,                     // 무한 루프
    max_duration_ms: u64,               // 매달린 CI job
    max_total_tokens: u64,              // 오타 하나에 $400
    max_context_tokens: u32,            // 윈도 초과
    max_consecutive_tool_errors: u32,   // 같은 깨진 명령 40번 재시도
}
```

각각은 그것이 없어서 누군가 데인 적이 있기 때문에 존재한다. `StopReason::LimitReached`는
**어느 한도가 걸렸는지** 운반한다. "그냥 멈췄습니다"는 런타임이 할 수 있는 가장 쓸모없는
말이다.

### 7.2 Tool call 은 순차 실행이 기본이다

MVP는 한 응답에 여러 tool call이 와도 **순차** 실행한다. 병렬 실행은 `read_only`
annotation을 신뢰해야 하는데, §4.2에서 그것을 신뢰하지 않기로 했다. 병렬화는 Policy가
`Allow`에 `parallel_safe` constraint를 붙이는 형태로 나중에 도입한다 (열린 질문 §11-3).

---

## 8. Plugin 아키텍처

### 8.1 계약

```rust
#[async_trait]
pub trait Plugin: Send + Sync + Debug {
    fn manifest(&self) -> PluginManifest;
    async fn load(&self, ctx: PluginContext) -> Result<PluginHandle>;
    async fn unload(&self, ctx: PluginContext) -> Result<()>;
}
```

Plugin이 하는 일은 하나다: **넘겨받은 registry에 capability를 등록한다.** 런타임에
손을 뻗지 않고, 상태를 바꾸지 않고, Agent Loop 참조를 들지 않는다. 이 제약이 같은 trait을
오늘은 in-process로, 나중에는 RPC/WASM으로 구현 가능하게 만든다.

### 8.2 소유권 추적이 lifecycle을 가능하게 한다

모든 등록은 **어느 plugin 인스턴스가 했는지**를 함께 기록한다. 이 필드 하나가
unload · hot reload · 로드 실패 롤백을 전부 가능하게 한다.

```text
load() 중 3개 등록 후 실패
   → registry.unregister_all(instance_id)
   → 부분 등록이 남지 않는다
```

이름 충돌은 **덮어쓰지 않고 거부한다.** 두 plugin이 모두 `shell`을 등록하는 것은 설정
버그이며, 에러 메시지는 양쪽 plugin 이름을 모두 말해야 한다.

### 8.3 권한은 교집합이다

```text
effective = manifest.permissions ∩ profile.permissions ∩ session overrides
```

Plugin은 자기 권한을 **넓힐 수 없다.** `readonly` 프로파일이 쓰기 가능한 plugin을
무장 해제시키는 방식이 이 교집합이다.

### 8.4 in-process → process → WASM

| Phase | 형태 | 신뢰 모델 | 시점 |
|---|---|---|---|
| 1 | Rust trait (in-process) | Trusted code | MVP |
| 2 | Process + JSON-RPC | Semi-trusted, OS 권한 분리 | Phase 6 |
| 3 | WASM | Capability-based | 이후 |

**처음부터 동적 ABI를 만들지 않는다.** Rust trait으로 시작해 계약이 실제 plugin으로
검증된 뒤에 프로토콜을 고정한다.

그 전환을 위해 지금 심어 둔 것이 `CapabilityVersion`이다. 매니페스트가 자신이 빌드된
계약 버전을 신고하고, host가 `accepts()`로 판정해 **등록 전에** 거부한다. major 0은
Cargo와 같이 모든 minor 변경을 breaking으로 취급한다.

---

## 9. Job Runtime

Rivet의 차별화 지점. 자세한 내용은 [`job.md`](./job.md).

```text
        PENDING ──► READY ──► RUNNING ──┬──► REVIEW ──┬──► COMPLETED
           │          ▲         │       │             │
           │          │         │       └──► FAILED   ├──► READY (변경 요청)
           │          └─────────┘                     └──► FAILED (반려)
           │        WAITING
           └────────────► CANCELLED  (모든 비종료 상태에서 가능)
```

세 가지 결정:

1. **종료 상태는 흡수 상태다.** `COMPLETED`에서 나가는 경로는 없다. 재개는 새 Job을
   만든다는 뜻이고, 그래야 이력이 정직하다.
2. **`RUNNING`에서 `COMPLETED`로 직행할 수 없다.** 반드시 `REVIEW`를 거친다. 게이트를
   건너뛸 수 있으면 게이트가 아니다. 리뷰가 필요 없는 워크플로는
   `Job::complete_without_review()`를 쓰는데, 이것도 `REVIEW`를 **통과**한다 — 로그에
   "열린 게이트"가 남지, "없는 게이트"가 남지 않는다.
3. **실패한 의존성은 `blocked()`로 드러난다.** 실패한 leaf 하나 때문에 그래프 전체가
   영원히 "작업 중"으로 보이는 상황을 막는다.
4. **`WAITING`은 종료가 아니다.** `is_settled()`는 `WAITING`을 살아있는 상태로 센다.
   사람 승인을 기다리며 파킹한 Job을 "끝남"으로 보면, approval-gate 워크플로는 매번
   사람이 답하기 전에 런타임이 종료된다.

### 상태 기계는 강제된다

`Job::state`와 `depends_on`은 **private**이다. 공개 필드였다면
`job.state = Completed` 한 줄로 리뷰 게이트가 무력화되고, `depends_on` 직접 수정으로
순환 검사가 우회된다. 유일한 진입점은 `JobGraph::apply()`와 `Job::transition_to()`이며
둘 다 검증한다.

재시도 예산도 전이에서 강제한다. `max_attempts` 소진 후 `RUNNING` 진입은 거부되므로,
어떤 workflow plugin도 예산을 두 번 쓸 수 없다.

`Workflow`는 그래프 모양에 대한 **순수 정책**이며 그래프를 변경하지 않는다. 상태 전이는
Job Runtime이 적용한다. 버그 있는 workflow plugin은 진행을 멈출 수는 있어도 상태를
오염시킬 수는 없다.

---

## 10. 워크스페이스 봉쇄

에이전트에서 가장 흔한 샌드박스 탈출은 경로 처리 실수다. 그래서 봉쇄를 각 tool이 아니라
`Workspace` 한 곳에서 강제한다.

```rust
ws.resolve("../etc/passwd")        // Err — 탈출
ws.resolve("src/../Cargo.toml")    // Ok  — 내부에 머무름
ws.resolve("/repo-secrets/key")    // Err — 접두사 유사 ≠ 내부
ws.resolve(".env")                 // Err — deny list
ws.resolve("sub/.env")             // Err — 이름만 쓴 패턴은 모든 깊이에 적용
ws.resolve("keys/server.pem")      // Err — glob (`**/*.pem`)
ws.resolve(".ENV")                 // Err — 대소문자 무시 FS 대응
```

```rust
ws.resolve(".ssh/id_rsa")          // Err — 디렉터리를 막으면 그 아래도 막힌다
```

**deny list는 glob이며 대소문자를 무시하고 서브트리를 포함한다.** 초기 설계는 경로
컴포넌트 리터럴 비교라 `**/*.pem`이 아무것도 막지 못했고, macOS(APFS) 기본 설정에서
`.ENV`가 `.env`를 그대로 열었다. glob으로 바꾼 첫 버전은 이번엔 `.ssh`는 막으면서
`.ssh/id_rsa`를 통과시켰다 — `.ssh`가 권장 목록에 있는 이유가 정확히 그 안의 키인데도.

패턴 하나는 세 가지로 확장된다: 그 자체, 그 아래 전부(`p/**`), 그리고 구분자가 없으면
모든 깊이(`**/p`, `**/p/**`). 문서가 광고하는 목록을 그대로 넣고 검증하는 회귀 테스트가
있다.

`FsScope::Subtree`도 같은 종류의 함정이 있었다. "좁은 쪽이 이긴다"는 meet 규칙 아래에서
`Subtree("../../../etc")`는 `Workspace` 프로파일과 만나 **자기 자신**이 되어, 워크스페이스
밖 권한을 발급했다. 원래의 정확 일치 버그보다 나쁘다. 이제 서브트리는 생성과 meet 양쪽에서
봉쇄 검증을 받는다.

**`resolve()`는 어휘적(lexical) 검사이며 그것만으로는 충분하지 않다.** 워크스페이스
*안의* 심볼릭 링크가 밖을 가리킬 수 있다. 런타임은 open 이후 재검증(`O_NOFOLLOW`,
post-open canonicalize)을 반드시 수행한다. 상세는 [`security.md`](./security.md).

---

## 11. 열린 질문

MVP 착수 전에 답이 필요한 것과, 의도적으로 미룬 것.

1. **컨텍스트 압축 전략** — `Checkpoint`가 이전 이력을 접는다는 것까지는 정했다.
   *언제* 접는지(토큰 임계? turn 수? 모델 판단?)와 요약을 누가 만드는지는 미정.
   Phase 1 종료 전 결정 필요.
2. **Prompt caching 경계** — provider별 cache breakpoint를 `ContextSlot` 경계와
   맞출 것인가. 비용에 직결되므로 Phase 1에서 측정 후 결정.
3. **Tool call 병렬 실행** — §7.2. `read_only` 주장을 신뢰하지 않기로 한 이상, 병렬화의
   안전 근거를 Policy에서 어떻게 표현할지 미정.
4. **대용량 payload** — 이미지·긴 tool 출력을 세션 로그에 인라인으로 넣으면 로그가
   비대해진다. content-addressed artifact store로 오프로딩하는 설계 필요. Phase 1에서
   `Truncation.artifact_ref` 자리만 잡아 두었다.
5. **다중 Run 동시성** — 한 워크스페이스에서 두 Job이 동시에 실행되면 파일이 충돌한다.
   git worktree 분리? 순차 강제? Phase 5 전 결정.
6. **Secret 취급** — `Permission::SecretsRead(keys)`만 정의했고 저장·주입·마스킹 경로는
   미설계. Phase 4.
7. **비-UTF-8 프로세스 출력** — `ExecOutput.stdout`이 `String`이라 임의 바이트를 내는
   프로세스를 표현할 수 없다. 현재는 lossy 변환 전제. `Vec<u8>` + 표시용 lossy 뷰로
   바꿀지 Phase 4에서 결정.
8. **Slot별 계약 버전 독립 진화** — `CapabilityKind::version()`이 아직 모든 슬롯에 대해
   `0.1`을 반환한다. 실제로 슬롯이 따로 움직이기 시작하는 Phase 6에서 구현.
9. **Provider egress와 tool egress가 같은 권한을 쓴다** — Phase 2에서 `manifest ∩ profile`이
   실제로 계산되기 시작하자, `NetworkHttp`를 주지 않는 프로파일은 `rivet.model-openai`가
   빈 권한이 되어 **에이전트를 아예 못 돌리게** 된다. 잠정 답으로 모든 프로파일이
   `NetworkHttp(None)`을 준다 (`config.rs:237`). 그 대가로 "네트워크 없는 프로파일"을
   어휘가 표현할 수 없게 됐고, `security.md` §8 표의 `network ✗` 열은 tool egress만
   가리키게 됐다. **Phase 4의 sandbox가 이걸 물려받기 전에 의도적으로 다시 열어야 한다** —
   provider 호출을 막는 프로파일이 필요하면 별도 permission이 필요하고 그건 `rivet-core`
   변경이다. 근거: [`design/phase-2-plugin-loader.md`](./design/phase-2-plugin-loader.md) §7-1.
10. **어느 프로파일도 주지 않는 permission이 넷이다** — `ProcessSpawn` · `SecretsRead` ·
   `EventsSubscribe` · `JobManage`. `Profile::permissions()`가 주는 것은
   `fs_read(workspace)` · `session_read` · `session_write` · `events_publish` ·
   `network_http`, 그리고 쓰기 프로파일의 `fs_write(workspace)`뿐이다. Phase 2 전에는
   교집합이 아무 데도 쓰이지 않아 무해했지만, 이제는 이 넷 중 하나를 선언한 매니페스트가
   **모든 프로파일에서** 빈 권한이 된다.
   - `EventsSubscribe` — **Phase 3의 `telemetry.log` plugin이 첫날 막힌다.**
   - `ProcessSpawn` — `security.md` §8 표는 `developer`·`ci`에 `process ✓`를 약속하지만
     주는 쪽이 없다. `plugin.md` §4.2가 "권한이 없으면 크게 실패한다"의 예로 `tool-shell`을
     들었었는데, 그렇게 쓰면 그 plugin은 `developer`에서도 로드되지 않는다.
   - `SecretsRead` — 파서가 비어 있지 않은 키 목록을 요구해 완성된 기능처럼 읽힌다.
     저장·주입 경로는 §11-6대로 Phase 4 미설계다.
   - `JobManage` — Phase 5까지 요청자가 없다.

   어느 프로파일이 무엇을 주는지는 정리가 아니라 보안 결정이므로 요청자가 생기는 Phase
   착수 시점(구독은 3, 시크릿·프로세스는 4)에 명시적으로 연다. 그때까지 문서 세 곳
   (`security.md` §8 각주, `plugin.md` §4.2, 여기)이 같은 사실을 말한다. 근거: 같은 문서
   §7-3, PR #1 리뷰 2라운드 finding 2.
11. **`Interceptor`에 대응하는 `CapabilityKind`가 없다** — manifest guard가
   `register_interceptor`를 선언된 슬롯에 매핑할 수 없어 잠정적으로 `capabilities = ["policy"]`를
   요구한다. 변형을 추가하는 것은 닫힌 어휘를 넓히는 `rivet-core` 변경이라 Phase 2 범위 밖으로
   뒀다. interceptor가 실제로 실행되는 Phase 4에서 결정. 근거: 같은 문서 §7-2.
12. **CLI에 남은 마지막 하드코딩 plugin id** — `Config::api_key_env()`가 자격 증명을 미리
   확인하려고 `[plugins."rivet.model-openai"]`를 직접 들여다본다. host가 특정 plugin의 설정
   키를 아는 것으로, Phase 2가 없앤 바로 그 종류의 결합이다. 더 나은 에러를 만들어 내므로
   남겨 뒀지만 부채다. 근거: 같은 문서 §7-7.
13. **`fs_read`의 scope는 아무것도 좁히지 않는다** — `tool-filesystem`은 `fs_write`만
   grant로 게이트하고 `read_file`·`list_dir`·`search`는 무조건 등록한다. 읽기를
   워크스페이스에 가두는 것은 grant가 아니라 `Workspace::resolve`와 fsguard다. 그래서
   `fs_read({ subtree = "docs" })`를 선언한 매니페스트도 워크스페이스 전체를 읽는
   `read_file`을 받고 `rivet plugin show`는 그것을 `granted`로 출력한다. 즉 읽기 scope는
   Phase 2에서 **선언**이며, 도구별 경로 범위를 실제로 강제하는 것은 Phase 4의 sandbox다.
   파서가 탈출 서브트리를 지금 거부하는 것은 그 강제가 붙을 때 어휘가 이미 정확하도록
   하기 위한 것이다. 근거: PR #1 리뷰 2라운드 question 1.

---

## 12. 아키텍처 결정 요약

| 결정 | 선택 | 근거 |
|---|---|---|
| Core의 역할 | 계약만, I/O 없음 | 계약 안정화가 목표 |
| Model 인터페이스 | 스트리밍 단일 경로 | UI가 두 경로를 구현하지 않도록 |
| Tool 실행 주체 | 런타임 (모델 아님) | Policy 삽입 지점 확보 |
| Policy 합성 | most-restrictive-wins, 결정론적 | 등록 순서에 의존하는 보안은 보안이 아님 |
| `PolicyDecision` 구조 | outcome / rewrite / constraints 분리 | 셋은 독립이며 합치면 서로를 삼킴 |
| rewrite 안전성 | 정체성 검사 + 파이프라인 재평가 | 넓히는 rewrite도 다시 판정됨 |
| 충돌하는 rewrite | `Deny` | 임의 선택은 다른 좁힘을 조용히 버림 |
| constraints 합성 | 축별 최소값 | 무의견 정책이 sandbox 요구를 못 지우게 |
| Interceptor 반환 | `RestrictiveDecision` (bare Allow 없음) | 확장 지점이 허용을 넓히지 못하게 |
| Interceptor 결과 | 단축 아님, fold에 합류 | 등록 순서가 결과를 바꾸지 못하게 |
| Unattended 승인 | `RequireApproval` → `Deny` | CI에서 매달리지 않음 |
| 확장 경로 | Subscriber / Interceptor / Policy | "무엇이 막을 수 있는가"를 열거 가능하게 |
| Bus 발행 | 동기, 무오류, lossy | 루프를 telemetry가 멈추지 못하게 |
| 유실 처리 | `SubscriberLagged` 보고 | 조용한 유실 금지 |
| Session | event-sourced, `Expect` | replay · audit · 동시쓰기 감지 |
| `append` 반환 시점 | fsync 이후 | 크래시가 전제인 시스템 |
| 중단된 tool call | replay가 합성 결과 생성 | 재개 불가능한 세션 방지 |
| 승인 | durable session event | 감사 · resume 후 일관성 |
| `ToolContext` | data + host 분리 | Phase 6에 plugin 재작성 없이 |
| Context | 예산 인식, 항목 단위 폐기 | 결정론 → cache · replay |
| 예산 초과 처리 | 우선순위 역전 금지 + key dedupe | greedy fill은 우선순위를 뒤집음 |
| 토큰 추정 | 문자 기반 + non-ASCII 가중 | `bytes/4`는 CJK를 2~4배 과소 추정 |
| 등록 | 소유권 추적, 충돌 거부 | unload · rollback · hot reload |
| 권한 | manifest ∩ profile | plugin이 자기 권한을 못 넓힘 |
| Sandbox | 보장을 스스로 신고 | 운영자를 오해시키지 않음 |
| `ExecSpec` env | 빈 상태에서 시작 | 시크릿 유출 기본 차단 |
| Job 종료 상태 | 흡수 | 이력의 정직성 |
| `Job.state` | private, 전이 함수만 | 공개 필드는 게이트를 장식으로 만듦 |
| Job 역직렬화 | `from_jobs()` 검증 경유 | private 필드도 상태 파일로는 우회 가능 |
| `READY → FAILED` | 허용 | 예산 소진 Job이 갈 곳이 있어야 함 |
| `WAITING` | 진행 중으로 계산 | 승인 대기 중 런타임 종료 방지 |
| Sandbox 해제 | `Drop` 아님, 런타임이 `teardown` 호출 | Rust `Drop`은 await 불가 |
| deny list | glob + 대소문자 무시 + 서브트리 | 디렉터리만 막으면 그 안의 키가 열림 |
| 권한 교집합 | `meet` (부분순서) | 정확 일치는 좁게 선언한 plugin을 벌함 |
| `Subtree` 검증 | 생성·meet 양쪽 | 탈출 서브트리가 프로파일을 넘어섬 |
| 승인 기억 | `scope_key` | 호출 id로는 영원히 매칭 안 됨 |
| 구독 등록 | 등록이 곧 attach | 두 단계면 조용히 아무것도 안 받음 |
| `RUNNING → COMPLETED` | 금지, `REVIEW` 경유 | 건너뛸 수 있는 게이트는 게이트가 아님 |
| Plugin ABI | Rust trait 우선, RPC 이후 | 계약 검증 전 프로토콜 고정 금지 |
| 버전 협상 | `CapabilityVersion` 사전 심음 | 외부 plugin 전환 대비 |
