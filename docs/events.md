# Event 모델

> 계약 버전 `0.1`

Rivet에는 **두 종류의 이벤트**가 있다. 이 둘을 섞는 것이 이 설계에서 가장 비싼 실수다.

---

## 1. Session Event vs Bus Event

| | Session Event | Bus Event |
|---|---|---|
| 성격 | durable fact | notification |
| 저장 | 항상 | 안 함 |
| 유실 | 절대 불가 | 허용 |
| 순서 | 조밀한 `seq` | best-effort |
| 소비자 | replay · resume · audit · fork | UI · telemetry · plugin |
| 실패 시 | Run 실패 | 조용히 진행 |
| 예 | `assistant.message` | `agent.text.delta` |

**판별 기준**: *"이것이 없으면 세션을 정확히 재구성할 수 없는가?"*
그렇다면 Session Event다. 아니면 Bus Event다.

`agent.text.delta`는 버스에만 흐른다. 세션은 **조립이 끝난 메시지**를 저장한다.
replay가 델타에서 메시지를 다시 유도할 필요가 없어야 결정론적이다.

---

## 2. Session Event

```text
session.created         workspace_root, parent(fork point)
run.started             run_id, agent_id, model, job_id?
user.message            message
model.requested         run_id, model, request_digest   ← 요청 *전에* 기록
assistant.message       run_id, message, stop_reason, usage
tool.called             run_id, call
tool.completed          run_id, call_id, result, duration_ms
tool.blocked            run_id, call_id, policy, reason
approval.requested      run_id, call_id, scope_key, reason, preview
approval.resolved       run_id, call_id, scope_key, outcome, remembered, actor
run.completed           run_id, stop, turns
session.checkpoint      summary, through_seq
session.closed          reason
```

### 세 가지 미묘한 점

**`model.requested`는 요청을 보내기 전에 기록한다.**
요청 도중 크래시하면 replay에서 "완료 없는 요청"으로 드러난다. 사후 기록이면 그 크래시는
흔적을 남기지 않는다.

**`tool.blocked`도 durable fact다.**
"모델이 요청했고 거부당했다"가 감사에서 정확히 필요한 사실이다. 또한 replay 시 거부
사유가 담긴 tool result를 모델에게 만들어 준다 — 없으면 모델이 같은 호출을 무한
재시도한다.

**`tool.called`는 messages로 투영되지 않는다.**
호출 자체는 직전 `assistant.message`의 `ContentBlock::ToolCall`에 이미 있다. 별도로
로깅하는 이유는 *디스패치가 시작된 시점*을 표시하기 위해서다. 투영까지 하면 재구성된
대화에 호출이 두 번 나타난다. 대신 replay는 이 이벤트로 **미해결 호출을 추적**한다.

**승인은 durable event다.**
`approval.requested` / `approval.resolved`가 로그에 남는다. "누가 언제 `rm -rf`를
승인했는가"는 감사가 정확히 묻는 질문이고, 버스는 유실될 수 있다. `remembered: true`인
`ApprovedForSession`은 로그의 projection이므로 resume 후에도 유지된다 — 메모리에만 두면
재개할 때마다 사용자에게 다시 묻게 된다.

**`scope_key`가 승인의 정체성이다.** 호출 id로 키를 잡으면 안 된다 — 이후의 모든 호출은
새 `ToolCallId`를 받으므로 "세션 동안 기억"이 durable하지만 **영원히 매칭되지 않는다.**
정책이 tool 이름과 실제로 관심 있는 것으로 키를 만든다(`shell:git-push`,
`write_file:src/**`).

```rust
if state.is_pre_approved("shell:git-push") { /* 다시 묻지 않는다 */ }
```

### 이름은 고정되어 있다

wire tag는 Rust 변수명에서 파생되지 않고 `#[serde(rename = "...")]`로 **못박혀 있다.**
세션 로그는 디스크 포맷이다. Rust에서 변수 이름을 바꾸는 것이 사용자가 가진 모든 세션
로그를 무효화해서는 안 된다. `tests::wire_names_are_pinned`가 드리프트하면 실패한다.

### 중단된 tool call

`tool.called` 다음에 `tool.completed`도 `tool.blocked`도 없이 로그가 끝나면, 재구성된
대화에는 **아무도 답하지 않은 tool call**이 남는다. provider는 이런 메시지 배열을 400으로
거부한다. `SessionState::replay()`는 이런 호출마다 `is_error: true` 합성 결과를 붙여
세션이 재개 가능하도록 만든다.

```rust
let state = SessionState::replay(&events);
assert!(state.pending_tool_calls().is_empty());  // replay 후에는 항상 비어 있다
```

### Replay

```rust
let state = SessionState::replay(&events);
```

`replay`는 순수 함수다. 시계도, I/O도, 전역 상태도 읽지 않는다. 같은 입력이면 항상 같은
출력. 이것이 audit·fork·evaluation의 전제다.

`Checkpoint`는 이전 이력을 접는다. replay는 처음부터가 아니라 마지막 checkpoint부터
시작할 수 있다.

### 동시 쓰기

```rust
store.append(session_id, Expect::Seq(last_seq), event).await?;
```

한 세션에 두 Run이 append하는 것은 버그다. `Expect::Seq` 불일치는 뒤섞인 로그가 아니라
**충돌 에러**로 드러난다.

`Option<u64>`가 아니라 enum인 이유: 동시성 보호를 포기하는 것(`Expect::Any`)이 작성자가
직접 타이핑한 단어여야 하기 때문이다. `None`은 너무 쉽게 지나친다.

**내구성**: `append`는 이벤트가 매체에 durable해진 뒤에만 반환한다(`fsync` 또는 등가).
버퍼링 후 즉시 반환하는 구현은 로그 꼬리를 잃는데, 그 크래시가 이 시스템이 존재하는
이유다.

---

## 3. Bus Event

### 토픽 목록

```text
agent.run.started              agent_id, model
agent.turn.started             turn
agent.request.started          model, input_tokens_estimate
agent.text.delta               text                      ← 저장 안 됨
agent.request.completed        usage, stop_reason, latency_ms
agent.request.failed           error, will_retry, attempt
agent.turn.completed           turn
agent.run.completed            turns, stop

tool.requested                 call_id, name
tool.policy.evaluated          call_id, decision(Box), policy
tool.approval.requested        call_id, reason
tool.approval.resolved         call_id, approved
tool.execute.started           call_id, name, sandboxed
tool.execute.progress          call_id, message           ← 저장 안 됨
tool.execute.completed         call_id, is_error, duration_ms
tool.blocked                   call_id, reason

job.created                   job_id, goal
job.state.changed             job_id, from, to, reason
job.run.attached              job_id, run_id, attempt
job.review.requested          job_id, reviewer
job.review.completed          job_id, verdict

plugin.discovered              plugin_id
plugin.loaded                  plugin_id, capabilities
plugin.load.failed             plugin_id, error
plugin.unloaded                plugin_id

runtime.started                version                    ← 호스트가 발행
runtime.shutting_down          reason                     ← 호스트가 발행
runtime.subscriber.lagged      subscriber, dropped
```

`runtime.started`와 `runtime.shutting_down`은 **호스트의 것**이다: 런타임을 시작한 쪽만
언제 시작했는지 알고, 정리하는 쪽만 왜 내리는지 안다. `rivet_runtime::lifecycle`의 두
함수가 그것을 발행하고, 순서가 계약이다 — `started`는 plugin discover **전에**,
`shutting_down`은 unload **전에**. 그래서 언로드되는 plugin의 구독자는
`runtime.shutting_down`까지는 보고 그 뒤의 `plugin.unloaded`는 못 본다.

**어느 것이 실제로 발행되는가.** 28개 중 20개는 이 트리에 발행 지점이 있고, 8개는 아직
없다 — `tool.policy.evaluated` · `tool.approval.requested` · `tool.approval.resolved`는
Phase 4, `job.*` 다섯은 Phase 5다. 그 분류를 코드 주석이 아니라 테스트가 붙든다:
`every_bus_topic_is_claimed`(`rivet-runtime/tests/event_flow.rs`)이 `Event::one_of_each()`를
**와일드카드 없는 두 층 `match`**로 훑어 각 변형을 "발행됨" 또는 "Phase N 대기"로 분류하고
두 기대 목록과 대조한다. 토픽이 새로 생기면 그 `match`가 컴파일에 실패한다.

토픽은 점으로 구분된 안정적 이름이다. 필터는 접두사 매칭이다.

```rust
fn topics(&self) -> Vec<String> {
    vec!["tool.".into(), "job.state".into()]
}
// "tool.execute.started" ✓   "job.state.changed" ✓   "agent.run.started" ✗
```

빈 목록은 "전부 구독"이다.

### 상관관계

```rust
EventEnvelope::new(payload).for_run(session_id, run_id)
```

모든 envelope는 `id` · `at` · `session_id?` · `run_id?`를 가진다. 여러 Run이 동시에
돌 때 이벤트를 갈래별로 나누는 근거다.

---

## 4. 발행은 막히지 않는다

```rust
pub trait EventBus: Send + Sync + Debug {
    fn publish(&self, envelope: EventEnvelope);   // async 아님, Result 아님
}
```

느린 telemetry 구독자에 의해 멈출 수 있는 Agent Loop는 **언젠가 반드시 멈춘다.**
그래서 `publish`는 동기이고 실패하지 않는다.

구독자가 없어서 `send`가 실패하는 것은 정상이며 에러가 아니다 — 듣는 사람이 없으니 잃은
것도 없다.

---

## 5. 유실은 조용하지 않다

버퍼(기본 4096)를 넘긴 구독자는 이벤트를 잃는다. 그러나 그 사실은 보고된다.

```text
runtime.subscriber.lagged { subscriber: "metrics", dropped: 1203 }
```

세 가지 선택지 중에:

| 방식 | 결과 |
|---|---|
| 발행자를 막는다 | 느린 telemetry가 에이전트를 멈춤 ✗ |
| 조용히 버린다 | telemetry가 말없이 과소 보고 ✗ |
| **버리고 보고한다** | 손실이 가시적 ✓ |

버퍼 4096은 스트림 델타 버스트를 흡수할 만큼 깊고, 멈춘 구독자가 한 시간이 아니라 한 turn
안에 드러날 만큼 얕다.

**호스트 자신의 관측자도 같은 보고를 받는다.** `attach`(plugin 펌프)와 `observe`(호스트
펌프)가 같은 함수를 쓴다 — `--jsonl`이나 TUI가 조용히 이벤트를 놓치는 동안 plugin의 유실만
보고된다면, 하필 그 스트림을 읽으라고 만든 모드에서 스트림이 거짓말을 하게 된다.

**보고는 유실 *구간*당 하나이지 `Lagged` 결과당 하나가 아니다.** 보고 자체가 `publish`이고,
꽉 찬 채널로의 `publish`는 가장 오래된 슬롯을 덮어쓰는데 `Lagged` 직후 수신자가 재배치되는
자리가 정확히 그 슬롯이다. 그래서 결과당 하나로 보고하면 **보고가 다음 랙을 만든다** —
측정: 용량 8 버스, 발행 40개가 118,312개가 될 때까지 아무도 이벤트를 못 받았다. 구간은
무언가 실제로 배달됐을 때 닫히고, 그 사이의 드롭은 보고되지 않는다. 그것들은 그 보고가 만든
드롭이다. (`a_lag_report_does_not_feed_itself_into_a_runaway`)

랙에 걸린 구독자는 **자기 랙 보고도 놓칠 수 있다.** TUI 상태바의 `dropped`가 `≥`로 표시되는
이유이고, 그 숫자는 "지금까지 본 보고의 합"이지 "잃은 것의 총계"가 아니다.

그리고 **누구의 손실인지 구분한다.** 보고는 랙에 걸린 구독자의 이름을 싣는다. 버스 위의 보고를
전부 더하면 느린 telemetry plugin의 드롭이 화면 자신의 드롭으로 읽히므로 — 다 받은 이벤트를
42개 놓쳤다고 말하게 된다 — 상태바는 자기 이름의 보고만 `≥N dropped`로 세고, 나머지는
`N elsewhere`로 따로 보여 준다. 둘 다 볼 값이지만 같은 값은 아니다.

---

## 6. Interceptor: 차단할 수 있는 유일한 구독자

| | `EventSubscriber` | `Interceptor` |
|---|---|---|
| 반환 | 없음 | `Option<PolicyDecision>` |
| 차단 | 불가 | 가능 |
| 개수 | 임의 | 열거·정렬·타임아웃 |

```rust
#[async_trait]
impl Interceptor for AuditGate {
    fn name(&self) -> &str { "audit-gate" }

    async fn before_tool_call(&self, req: &PolicyRequest) -> Result<Option<PolicyDecision>> {
        if self.is_after_hours() {
            return Ok(Some(PolicyDecision::Deny { reason: "outside change window".into() }));
        }
        Ok(None)   // 나머지는 Policy chain 에 위임
    }
}
```

Interceptor는 Policy 평가 **앞에** 돈다. 그러나 `Some`이어도 **단축하지 않는다** — 결과는
Policy chain과 같은 `combine()` fold에 합류하고, 더 제한적인 쪽이 이긴다.

반환 타입이 `RestrictiveDecision`이라는 점이 핵심이다. `Allow` 변형이 **존재하지 않으므로**
interceptor는 결과를 더 엄격하게만 만들 수 있다. 단축을 허용하고 `Allow`를 허용했다면,
이름이 `acme-allow-all`인 plugin이 `zz-deny`를 알파벳순으로 이기는 — 즉 등록 순서가 보안
결정을 좌우하는 — 구조가 된다.

`priority()`는 사용자가 **어느 이유를 먼저 보는지**만 정한다. 결과는 바꿀 수 없다.

런타임이 타임아웃을 적용하며, 멈춘 interceptor는 `None`으로 처리되고 보고된다.

---

## 7. 소비자 예

### TUI

```rust
// `rivet-core` 만 본다. `EventSubscriber` 는 core 의 계약이고,
// `BroadcastBus` 는 런타임 타입이므로 TUI 는 그것을 이름조차 모른다.
#[async_trait]
impl EventSubscriber for Tui {
    fn name(&self) -> &str { "render.tui" }

    async fn on_event(&self, envelope: &EventEnvelope) {
        self.state.lock().unwrap().apply(envelope);   // 순수 fold
    }
}

// 붙이는 것은 호스트다:  bus.observe(tui.clone())
```

TUI는 런타임 타입을 하나도 import 하지 않는다. **이벤트만 소비하는 클라이언트**다.
`crates/rivet-tui/Cargo.toml`에 `rivet-runtime`이 없으므로 이것은 lint가 아니라
**컴파일 성질**이고, `the_tui_crate_does_not_depend_on_the_runtime`이 그 매니페스트를
파싱해 되돌리는 편집을 막는다.

> 이전 판의 예시는 `bus.subscribe_raw()`를 쓰면서 두 줄 아래에서 "TUI는 런타임 타입을
> 하나도 import 하지 않는다"고 적고 있었다. `subscribe_raw`는 `BroadcastBus`의
> 메서드다 — 문서가 자기와 모순이었고, Phase 3이 그것을 고쳤다.

### JSONL (CI 통합)

```bash
rivet --jsonl "run the tests" | jq -c 'select(.payload.event == "tool")'
```

`--jsonl`은 **관찰 가능성**을 위한 것이지 세션 재구성용이 아니다. 버스는 lossy이므로
유실될 수 있는 스트림으로 durable 로그를 재구성할 수 없다. 재구성이 필요하면 세션 로그
자체를 읽어야 한다 (`rivet session show --json`).

스트림은 `runtime.started`로 시작해 `plugin.discovered` · `plugin.loaded`를 싣고,
`runtime.shutting_down` · `plugin.unloaded`로 끝난다 — 관측자가 plugin 로드 **전에**
붙기 때문이다. 꼬리는 시계가 아니라 채널이 정한다: 발행이 끝난 뒤 호스트가 채널이 빌
때까지(예산 2초) 배달하고, 예산을 넘기면 "잘렸다"고 stderr에 한 줄 남긴다. 잘린 것을
모르는 스트림보다 잘렸다고 말하는 스트림이 낫다.
(`jsonl_carries_the_whole_lifecycle_not_just_the_answer` ·
`the_jsonl_stream_is_not_a_session_export`)
