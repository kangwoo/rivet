<!-- Phase 4b를 위해 승인받으려는 설계. 구현 전에 쓴다.
     §9는 구현이 이 문서에서 갈라진 지점을 나중에 기록하는 자리이고,
     §9 위의 본문은 나중에 선견지명처럼 보이도록 고치지 않는다. -->

# Phase 4b — Conversation: 설계

> 상태: 설계 (구현 전) · 구현 대상: [`docs/plan.md`](../plan.md) `## Phase 4b — Conversation`
> (4b.1–4b.8, DoD 10줄, 위험 요소 7건, 그리고 [`### 4b.8` 표](../plan.md#4b8--이-phase가-끝나는-날-거짓이-되는-문장))
> 이 문서는 **결정과 그 근거**를 남긴다. 무엇을 만들었는지는 커밋이, 계약의 현재 모습은
> [`architecture.md`](../architecture.md) · [`events.md`](../events.md) ·
> [`config.md`](../config.md) · [`plugin.md`](../plugin.md) · [`security.md`](../security.md)가
> 말한다.
>
> 교차 Phase 파급이 있는 열린 질문은 [`architecture.md` §11](../architecture.md#11-열린-질문)로
> 승격한다. §7에 남은 것은 이 Phase 안에서 닫히는 것들이다.

> 범위: `docs/plan.md`의 Phase 4b 절 전부. 기준 트리: `herdr/phase-4b-conversation-design`
> (base `a41d115`). 계획서가 이미 확정한 것 — turn 사이의 상태는 **로그에서 다시 읽는다**(B가
> 아니라 A), 사용자 turn은 **이벤트로 온다**(로컬 에코가 아니라), Phase 번호를 밀지 않는다 —
> 은 전제이지 논의 대상이 아니다. 계획서의 `### 4b.8` 표(산문 열다섯 곳)는 **다시 만들지
> 않는다**; §3.6이 그것을 참조하고 §8이 세 행을 보탠다.
>
> 이 문서가 인용하는 `file:line`은 전부 위 base에서 실제 파일과 대조했다. 방법은 §6.0에
> 적었다.

---

## 1. 문제

Phase 4b가 만들 것은 새 기능이 아니라 **수명 하나**다. `--tui`의 화면은 run 하나에 묶여
있고, 그 묶임은 `crates/rivet-cli/src/run.rs`의 `drive`에 직선으로 적혀 있다:
화면을 켜고([`run.rs:283-286`](../../crates/rivet-cli/src/run.rs)), `agent_loop.run(...)`을
**한 번** 돌리고([`run.rs:321`](../../crates/rivet-cli/src/run.rs)), 화면을 내리고
([`run.rs:344-346`](../../crates/rivet-cli/src/run.rs)), 사라진 대체 화면 대신 답을 다시
찍는다([`run.rs:356-358`](../../crates/rivet-cli/src/run.rs)의 `answer`). 후속 프롬프트를 칠
입력창은 존재하지 않는다 — `Intent`에는 변이가 둘뿐이고(`Quit`·`Cancel`,
[`app.rs:30-37`](../../crates/rivet-tui/src/app.rs)) 둘 다 사용자가 **끝내려는** 의사다.

여덟 군데가 비어 있고, 성격이 셋으로 갈린다.

**첫째 — 없는 것 셋.** ① 화면에서 호스트로 가는 **둘째 왕복**이 없다. 첫째(승인)는 Phase 4가
`ApprovalSink`로 놓았고([`app.rs:295-341`](../../crates/rivet-tui/src/app.rs)), 그것이
`AppState::apply`가 순수 fold라는 성질에 낸 유일한 구멍이다
([`state.rs:152-158`](../../crates/rivet-tui/src/state.rs)). 사용자 입력에는 그 구멍이 필요
없다 — 답을 놓을 데가 이미 있다. ② 입력 모드가 없다. `handle_key`는 모든 키를 명령으로 읽고
([`app.rs:242-258`](../../crates/rivet-tui/src/app.rs)), 그중 넷(`c`·`q`·`Esc`·`Tab`)이
타이핑과 정면으로 부딪힌다. ③ 사용자 turn을 나르는 버스 토픽이 없다. durable한 절반은 이미
있다 — `SessionEvent::UserMessage`는 `AgentLoop::run`이 매번 적는다
([`agent_loop.rs:199-206`](../../crates/rivet-runtime/src/agent_loop.rs)) — 관찰 쪽 쌍둥이만
빈칸이고, 그 빈칸은 [`events.md:9-22`](../events.md)가 그린 이중 채널의 한쪽이다.

**둘째 — 있는데 아무도 안 읽는 것 하나.** `AssembledContext.dropped`는 무엇을 버렸는지 이미
세어 담고, 그 doc이 "so a UI can warn about both"라고 적었는데
([`context.rs:150-152`](../../crates/rivet-core/src/context.rs),
[`context/mod.rs:221-231`](../../crates/rivet-runtime/src/context/mod.rs)), 프로덕션
독자가 0이다 — 워크스페이스 전체에서 `assembled.dropped`를 읽는 코드는 `context/mod.rs`의
테스트 둘뿐이다. 한 번 뜨고 마는 화면에서는 넘어갔다. 대화형 화면에서는 사용자가 위로 스크롤해
읽을 수 있는 transcript를 모델은 더 이상 갖고 있지 않게 되고, 아무도 말해 주지 않는다.

**셋째 — 화면이 오래 살면 거짓이 되는 것 넷.** ① `RunConfig.cancel`은 `drive`가 하나만 만들어
([`run.rs:273`](../../crates/rivet-cli/src/run.rs)) 신호 처리기·`Screen`·`cfg`에 나눠 주는데,
`CancellationToken`은 래치이고 `Watchdog::start`가 `cfg.cancel.child_token()`으로 소비한다
([`agent_loop.rs:762`](../../crates/rivet-runtime/src/agent_loop.rs)) — 첫 Ctrl-C 이후의 모든
turn이 모델을 부르기도 전에 취소된 채로 태어난다. ② `asked_to_cancel`은 펌프 태스크의 지역
변수이고([`run.rs:456`](../../crates/rivet-cli/src/run.rs)) 펌프는 한 번만 spawn되므로
([`run.rs:452-463`](../../crates/rivet-cli/src/run.rs)) turn 2의 **첫** Ctrl-C가 `ForceExit`가
된다. ③ `AppState`의 `run.retries`·`run.last_error`는 run 단위인데 `RunStarted` 팔이
`run.stop`만 지운다([`state.rs:184-187`](../../crates/rivet-tui/src/state.rs)). ④
`store.read(session_id, 1, 1_000)`([`run.rs:81`](../../crates/rivet-cli/src/run.rs))은 이벤트가
하나뿐인 새 세션에서는 무해하고, turn마다 반복하면 1,000을 넘긴 대화의 앞부분을 조용히 자른다.

**그리고 이 Phase가 만들 것의 절반은 삭제다.** `answer()`
([`run.rs:380-400`](../../crates/rivet-cli/src/run.rs))는 "화면이 너무 일찍 죽는다"를 메우던
처치이고, 그 함수가 필요 없어지는 것이 이 Phase가 필요했다는 증거다.

Phase 4b는 이 여덟을 닫는다. 그리고 닫으면서 **화면이 이벤트만 소비한다**는 성질
([`lib.rs:16-21`](../../crates/rivet-tui/src/lib.rs))과 **로그가 단일 진실 원천**이라는 성질
([`architecture.md:333-335`](../architecture.md))을 둘 다 깨지 않는다.

---

## 2. 접근

### 2.1 모양

```text
  rivet --tui "…"
        │
   observe()  ── Tui::new() ─▶ (control: Receiver<Intent>, prompts: Receiver<Prompt>)
        │                            │                          │
        ▼                            │                          │
  ┌──── drive() ────────────────────────────────────────────────────────────────┐
  │  루프 밖 (대화 하나에 한 번)                                                  │
  │    agent_spec · ContextAssembler · sink · AgentLoop · Screen::start          │
  │                                   │                          │              │
  │                          ┌────────┴────────┐                 │              │
  │                          │  펌프 태스크     │◀── control ─────┘              │
  │                          │  CurrentTurn    │                                │
  │                          └────────┬────────┘                                │
  │                                   │ react(intent) → (Reaction, token)       │
  │  ┌──── converse() ─────────────────────────────────────────────────────┐    │
  │  │  loop {                                                             │    │
  │  │    input ← 첫 turn이면 FirstTurn, 아니면 prompts.recv().await        │◀───┼── prompts
  │  │            └─ None(= 화면이 갔다) ─▶ break                            │    │
  │  │    state ← close_interrupted(store, id)   ← 수리 + 끝까지 읽기        │    │
  │  │    tui.sync_transcript(&state.messages)   ← 버스가 놓친 것 보수       │    │
  │  │    turns.run(id, state, input)            ← CurrentTurn::begin()     │    │
  │  │                                             signals::install         │    │
  │  │                                             RunConfig 새로           │    │
  │  │    last = Some(summary)                                             │    │
  │  │  }                                                                   │    │
  │  └──────────────────────────────────┬──────────────────────────────────┘    │
  │  루프 밖 (teardown, 지금의 순서 그대로)                                       │
  │    host.shutdown() → shutting_down → unload_all → watching.finish()          │
  │      → screen.stop() ⇒ Restored → report(&restored, &last)                   │
  └─────────────────────────────────────────────────────────────────────────────┘
```

설계를 지탱하는 결정 여덟.

**(a) 펌프는 남기고, 펌프가 보는 것에 한 겹을 끼운다. 그 한 겹은 `CurrentTurn` 하나이고,
토큰과 `asked` 플래그를 **같은 값에** 담는다.** 계획서의 위험 요소는 "펌프가 보는 것을 turn마다
갈아 끼울 수 있게 하라"까지 말하고 자리를 정하지 않았다. 자리는 `rivet-cli`의 `run.rs`이고,
모양은 `Arc<Mutex<TurnState>>` 한 겹이다 (§4.3).

두 필드를 **한 값에** 두는 것이 이 결정의 요점이다. 계획서는 "수리·취소 토큰·취소 플래그는
turn마다"라고 셋을 나란히 적었는데, 뒤의 둘은 나란히 두면 **어긋날 수 있다**: 토큰만 갈고
플래그를 안 갈면 turn 2의 첫 Ctrl-C가 `ForceExit`이고, 플래그만 갈고 토큰을 안 갈면 turn 2의
Ctrl-C가 turn 1의 죽은 토큰을 취소한다 — 계획서가 "래치 버그보다 나쁘다"고 적은 바로 그것이다.
`CurrentTurn::begin()`이 둘을 **한 번의 잠금 안에서** 함께 갈므로 어긋나는 상태가 표현될 수
없다.

**펌프를 지우고 `drive`가 `select!`로 직접 받는 안은 기각한다.** 그 안이 매력적인 이유는
분명하다 — 간접이 통째로 없어지고 토큰이 늘 스코프 안에 있다. 그러나 펌프가 별도 태스크인
이유는 취소가 **호스트 자신이 멈췄을 때도** 동작해야 한다는 것이고, 오늘 그 경우가 실재한다:
`Plugin::unload`에는 데드라인이 없다([`architecture.md` §11-15](../architecture.md), Phase 2가
남기고 Phase 3이 다시 적은 것). `host.loader.unload_all()`
([`run.rs:332`](../../crates/rivet-cli/src/run.rs))이 매달리면 `select!` 안 이었던 `drive`는
키를 못 읽고, raw mode의 터미널은 두 번째 Ctrl-C로도 빠져나올 수 없게 된다. 펌프는 그 경우의
유일한 출구다.

**(b) 취소 토큰은 turn마다 새로 만들고, 신호 처리기는 turn 루프 **안**으로 들어간다.**
`signals::install`([`run.rs:274`](../../crates/rivet-cli/src/run.rs))과 그 `abort`
([`run.rs:323`](../../crates/rivet-cli/src/run.rs))이 둘 다 `drive` 안에 있으므로, 통째로
`Turns::run` 안으로 옮기면 매 turn 그 turn의 토큰을 집는다. `signals.rs`는 한 줄도 바뀌지
않는다. 부작용 하나를 정직하게 적는다: teardown 동안에는 신호 처리기가 없다. 그것은 **오늘도
그렇다** — `signals.abort()`가 `unload_all()`보다 먼저 온다 — 이므로 회귀가 아니다.

**"취소되지 않는 부모를 오래 두고 turn마다 자식을 낸다"는 답이 아니다.** 취소하는 것이
펌프이므로 펌프가 부모를 쥐면 첫 Ctrl-C가 부모를 래치하고 이후 모든 자식이 취소된 채 태어난다.
`CurrentTurn`이 쥐는 것은 **부모가 아니라 지금 turn의 토큰 그 자체**다.

**(c) 프롬프트는 유실될 수 있는 채널을 타지 않는다 — 채널을 나눈다.** `mpsc::channel(8)` +
`try_send`([`app.rs:78`](../../crates/rivet-tui/src/app.rs)·
[`:259-264`](../../crates/rivet-tui/src/app.rs))의 근거는 코드가 스스로 적어 두었다:
"a full channel means the host is not reading… Dropping the duplicate is right — the channel
already holds an unread intent of its own." 그 근거는 **멱등한 것에만** 성립한다. `Quit`을 두
번 보내는 것과 한 번 보내는 것은 같고, 사용자가 방금 친 문장을 두 개 보내는 것과 하나
보내는 것은 다르다.

그래서 `Tui::new()`는 채널 **둘**을 낸다. 제어(`Intent`)는 지금 그대로 용량 8 + `try_send`이고
근거도 그대로다. 프롬프트(`Prompt`)는 용량 1의 자기 채널을 타고, 그 채널이 꽉 차는 유일한
경우는 "호스트가 아직 직전 프롬프트를 안 집었다"뿐이다 — 그때 컴포저는 초안을 **지우지 않고**
화면에 남긴다. 즉 어느 경로로도 사용자가 친 문장이 조용히 사라지지 않는다.

**`send().await`는 기각한다.** `handle_key`는 동기 함수이고 렌더 루프의 태스크에서 불린다
([`app.rs:190-192`](../../crates/rivet-tui/src/app.rs)). 거기서 await하면 채널이 찬 동안
**다시 그리기가 멈춘다** — 사용자가 기다리고 있는 바로 그 일이. `try_send` 주석이 그 이유를
이미 적어 두었고, 그 문장은 프롬프트에도 그대로 참이다.

**그리고 나눈 채널은 대화의 끝을 공짜로 준다.** `Tui`가 프롬프트 **송신단**을 들고
`dismiss()`가 그것을 떨어뜨리면, `prompts.recv()`가 `None`을 돌려주는 것이 곧 "화면이 없다"는
사실이 된다. `q`가 그리기만 끝내고 대화는 계속되는 오늘의 버그(DoD 6)가, 이미 존재하는
`dismiss()` 경로 — `Reaction::StopDrawing`([`run.rs:550-553`](../../crates/rivet-cli/src/run.rs)),
`Screen::stop`([`run.rs:481`](../../crates/rivet-cli/src/run.rs)),
`Screen::drop`([`run.rs:502`](../../crates/rivet-cli/src/run.rs)),
`Dismissal`([`app.rs:268-275`](../../crates/rivet-tui/src/app.rs)) — 전부에서 한꺼번에 닫힌다.
"화면이 없으면 sink가 아니다"와 "화면이 없으면 물을 프롬프트도 없다"가 **같은 한 사실**이 된다.

**(d) 모드는 둘이고, 승인 모달은 컴포저보다 세다.** `Watching`이 기본이고 오늘의 키 맵 그대로다.
`Composing`에서 `c`·`q`는 문자이고 `Esc`는 컴포저를 나가는 것이며 `Tab`은 아무것도 아니다
(§4.4의 표). Ctrl-C만은 두 모드·모달 유무와 무관하게 `Cancel`이다.

**모달이 컴포저보다 센 방향이어야 한다.** 반대로 하면 — 컴포저가 떠 있는 동안 `y`·`n`이 문자가
되면 — 승인이 필요한 호출이 `max_duration_ms` 동안 답을 못 받는다. 그건 Phase 4의 PR 리뷰가
"셋째 문"이라고 부르며 닫은 바로 그 실패다(`docs/plan.md` 진행 추적 표의 Phase 4 행). 그래서
`Tui::request`가 모달을 올릴 때 컴포저를 **일시정지**시킨다(초안은 그대로 두고 모드만
`Watching`으로), 모달이 내려갈 때 되돌린다. DoD의
`an_approval_modal_does_not_eat_the_composer`가 단언하는 것이 정확히 이것이다 — 모달은 컴포저를
**먹지 않고 미룬다**.

`answer_approval`이 키를 먼저 집는 순서([`app.rs:239-241`](../../crates/rivet-tui/src/app.rs))는
그대로 두되, 그 앞의 예외를 수정한다. 오늘 그 줄은 `KeyCode::Char('c')`를 **수식자와 무관하게**
모달에서 빼는데, `Composing`에서 맨 `c`는 문자여야 한다. 그러므로 예외는 "Ctrl-C, 그리고
`Watching`에서의 맨 `c`"가 된다 (§4.4).

**(e) transcript는 `RunView.text`가 아니라 새 자료구조이고, 대화 단위다.** `RunView`는 이름
그대로 run 단위다 — `stop`·`retries`·`last_error`·`turn`이 그렇다. 반면 사용자가 위로 스크롤해
읽는 것은 대화 단위다. 둘을 한 필드에 얹으면 "turn 경계에서 무엇을 지우는가"에 답이 없다.
그래서 `run.text`·`run.text_dropped`는 **`AppState.transcript`로 옮기고**, `RunView`에는 run
단위만 남긴다. `RunStarted` 팔이 지우는 것은 `stop`·`retries`·`last_error`이고
(DoD 7이 이름을 댄 둘이 여기 있다), transcript는 건드리지 않는다.

**보수 경로가 로컬 에코의 재발명이 아닌 이유.** §2.2가 로컬 에코를 "순수 fold에 둘째 예외를
뚫는다"로 기각했는데, 채택한 `sync_transcript`/`rebuild_transcript`도 fold 밖에서
`AppState.transcript`를 쓴다. 셋이 다르다. ① **출처**: 로컬 에코의 출처는 키 입력이고 이쪽은
로그다 — [`architecture.md:335`](../architecture.md)가 단일 진실 원천으로 못박은 그것.
② **연산**: 로컬 에코는 fold의 출력에 항목을 **끼워 넣으므로** 순서·중복 규칙을 발명해야 하고,
이쪽은 **전체 함수의 결과로 갈아 끼우므로** 발명할 규칙이 없다(멱등이고, 두 번 돌려도 같다).
③ **시점**: 아무것도 스트리밍되고 있지 않은 turn 경계에서만 돈다. 그래서
`the_transcript_shows_the_user_turn_from_events_alone`은 **여전히 성립해야 한다** — fold만으로
그려지는 것이 계약이고, 이 함수는 버스가 떨어뜨렸을 때의 복구지 대체가 아니다.

**링 버퍼의 성질은 그대로 옮긴다.** 문자 단위로 세고 바이트로 자르는
`push_text`의 조심성([`state.rs:345-364`](../../crates/rivet-tui/src/state.rs))은 CJK 출력에서
4배까지 과장되던 것을 고친 결과이고, 그 교훈을 지우면
`what_it_says_it_dropped_is_characters_not_bytes`가 지킬 것이 없어진다. 그러므로 새 버퍼도
**앞에서 문자를 덜어내고 문자 수를 보고한다**. 상한만 8 KB에서 64 KB로 올린다 — 범위가 run에서
대화로 넓어졌으므로.

**(f) `dropped`를 화면에 올리려면 그 수가 이벤트에 있어야 한다. 오늘은 문자열 안에 있다.**
계획서는 "이미 세어 둔 수를 그리는 일"이라고 적었는데, 실제로 세어 둔 모양은
`DroppedItem { key: "history.turns:3", slot: History, tokens: 0 }`이다
([`context/mod.rs:222-230`](../../crates/rivet-runtime/src/context/mod.rs)) — UI가 읽으려면
문자열에서 숫자를 파싱해야 한다. 그건 세어 둔 것이 아니라 적어 둔 것이다.

그래서 두 줄을 고친다: `AssembledContext`에 `dropped_turns: u32`를 더하고,
`dropped_report`의 합성 항목을 지운다. 그리고 그 수를 `AgentEvent::RequestStarted`에 **필드로**
싣는다 — 새 토픽이 아니라 기존 토픽의 필드다. 새 토픽이면 어휘 확장이고 트립와이어 둘을 다시
지나야 하는데, 이 수는 이미 "이 요청이 무엇으로 조립되었는가"를 말하는 토픽에 속한다.

**(g) 새 토픽은 하나이고, `include_conversation`은 둘을 덮는다.** `agent.input.received`는
`readonly`·`reviewer`·`production`에 **닫힌 채로** 도착한다 — 일곱 prefix 어디에도 걸리지 않고
([`config.rs:334-352`](../../crates/rivet-cli/src/config.rs)), `config.rs:329-332`가 그 경우를
"a prompt echo, say"라는 이름으로 미리 적어 두었다. 그 닫힘을 우연이 아니라 **답변**으로 만드는
것은 컴파일 트립와이어 둘이다: `every_bus_topic_is_claimed`
([`event_flow.rs:73`](../../crates/rivet-runtime/tests/event_flow.rs))과
`every_topic_is_granted_or_deliberately_withheld`
([`config.rs:680`](../../crates/rivet-cli/src/config.rs)). 후자의 답은 `false`이고 이유는
`TextDelta`와 같다.

`telemetry-log`의 `CONVERSATION_TOPIC`은 상수 **하나에서 배열 하나로** 바뀐다
(`CONVERSATION_TOPICS: [&str; 2]`). 별칭을 남기지 않는 것이 요점이다 — 남기면 여덟 개의 사용
지점 중 어느 것이 갱신되었는지 컴파일러가 말해 주지 않는다. `include_conversation`이 한 플래그로
양쪽을 덮는 이유는 "대화를 로그에 넣는다"가 **한 질문**이고, 그것에 절반만 답하는 것이 이 상수가
막으려고 존재하는 실패이기 때문이다.

**(h) 여러 turn의 종료 코드는 마지막 turn이 정하고, `resume --tui`는 물어볼 수 있게 된다.**
`finish`는 `RunSummary` 하나를 접는다([`main.rs:330-335`](../../crates/rivet-cli/src/main.rs)).
N turn은 N개의 요약을 내고, 프로세스의 코드는 **마지막 것**이다 — 사용자가 turn 1의 한도를
보고 다른 것을 물어 끝냈다면 프로세스가 실패로 끝날 이유가 없다 (§2.2가 "worst wins"를 기각한
이유). 그리고 `resume_plan`의 `Refuse`는 "`rivet resume`은 새 프롬프트를 받지 않는다"를 근거로
쓰였는데([`session_recovery.rs:277-280`](../../crates/rivet-runtime/src/session_recovery.rs)),
`--tui`는 그 근거를 없앤다. 게다가 **대화가 정상적으로 끝난 세션의 상태가 바로 그 `Refuse`
경우다**(마지막 메시지가 tool call 없는 assistant) — 그대로 두면 `rivet resume --tui`가 가장
흔한 경우에 쓸모없다. 그래서 변이를 하나 더한다: `ResumePlan::AskForMore`.

### 2.2 기각한 대안

| 대안 | 기각 이유 |
|---|---|
| **펌프를 지우고 `drive`가 `select!`로 intent를 직접 받는다** | 간접이 없어지는 대신, 호스트 자신이 매달렸을 때의 유일한 출구가 없어진다. `Plugin::unload`에 데드라인이 없고([`architecture.md` §11-15](../architecture.md)) `unload_all()`은 teardown 한가운데 있다([`run.rs:332`](../../crates/rivet-cli/src/run.rs)) — 거기서 매달리면 raw mode 터미널을 두 번째 Ctrl-C로도 못 벗어난다 (§2.1-a). |
| **취소되지 않는 부모 토큰을 오래 두고 turn마다 자식을 낸다** | 취소하는 것이 펌프이므로 펌프가 부모를 쥔다. 첫 Ctrl-C가 부모를 래치하면 이후 자식은 전부 취소된 채 태어난다 — 되돌아온 원래 버그다. |
| **`cfg`에만 새 토큰을 주고 펌프는 그대로 둔다** | 펌프가 turn 1의 죽은 토큰을 계속 취소한다. turn 2부터 Ctrl-C가 **아무 일도 하지 않는다** — 래치 버그보다 나쁘다(그쪽은 적어도 멈추기는 한다). |
| **토큰과 `asked_to_cancel`을 각각 따로 갈아 끼운다** | 둘이 어긋나는 상태가 표현 가능해진다. 한 값·한 잠금이면 어긋날 수 없다 (§2.1-a). |
| **`Intent::Prompt(String)`을 기존 채널 하나에 싣는다** | 그 채널의 규율이 `try_send`이고 그 근거가 "중복은 버려도 된다"인데, 고유한 내용에는 성립하지 않는다. 변이별로 규율을 나누면 한 채널 안에 두 계약이 생긴다. |
| **`Intent::Prompt`을 `send().await`로 보낸다** | `handle_key`는 렌더 루프 태스크의 동기 함수다([`app.rs:190-192`](../../crates/rivet-tui/src/app.rs)). 거기서 await하면 채널이 찬 동안 다시 그리기가 멈춘다 — `try_send`를 고른 원래 이유가 그것이다. spawn해서 보내면 순서가 뒤집혀 프롬프트 뒤에 친 Ctrl-C가 먼저 도착할 수 있다. |
| **프롬프트 채널을 unbounded로 둔다** | 유실은 없지만 이 crate가 스스로 세운 규칙("Everything that grows is bounded", [`state.rs:8-10`](../../crates/rivet-tui/src/state.rs))을 깬다. 용량 1 + "못 넘긴 초안은 화면에 남는다"가 같은 보장을 유계로 준다. |
| **`Intent`에 `Prompt(String)` 변이를 더한다** | 펌프가 **절대 볼 수 없는** 변이가 `Reaction::to`의 와일드카드 없는 `match`에 팔을 하나 갖게 된다. 그 팔에 쓸 정직한 답이 없다. 타입을 나누면 "두 채널이 교차했다"가 표현 불가능해진다 (§8-4). |
| **컴포저가 승인 모달보다 세다** | `y`·`n`이 문자가 되면 승인이 답을 못 받고 run이 `max_duration_ms`를 다 쓴다. Phase 4의 PR 리뷰가 닫은 "셋째 문"이 다시 열린다. |
| **transcript를 `RunView.text`에 그대로 얹는다** | `RunView`는 run 단위이고 transcript는 대화 단위다. 한 필드에 얹으면 "turn 경계에서 무엇을 지우는가"에 답이 없어지고, DoD 7의 `a_new_turn_clears_what_belonged_to_the_last_one`이 지킬 대상을 잃는다. |
| **transcript를 항목(entry) 단위로 버린다** | 보고 단위가 "N개의 메시지"가 되면서 `what_it_says_it_dropped_is_characters_not_bytes`가 지키던 성질(바이트로 자르고 문자로 보고한다)이 사라진다. 그 테스트는 실제 결함을 잡아 만들어진 것이라 없앨 것이 아니다. |
| **도구 줄을 transcript 항목으로 섞어 시간순으로 그린다** | 채팅 UI로는 더 낫지만, `tool_index`가 항목 배열을 가리키게 되고 퇴출 때마다 인덱스를 옮겨야 한다 — 계획서가 "4b.5가 인정하는 것보다 큰 변경"이라고 적은 바로 그 방향이다. §7-3에 남긴다. |
| **`dropped`를 새 토픽(`agent.context.trimmed`)으로 낸다** | 이 Phase가 이미 토픽 하나를 더한다. 두 번째는 트립와이어 둘·프로파일 목록·telemetry 기본 목록·문서 넉 줄을 다시 지나야 하는데, 그 수는 "이 요청이 무엇으로 조립되었는가"를 이미 말하는 토픽에 속한다 (§2.1-f). |
| **버스 유실을 화면이 로컬 에코로 메운다** | `AppState::apply`가 순수 fold라는 성질에 **둘째 예외**를 뚫는다. 첫째 예외(`pending`)는 "답을 놓을 데가 없다"로 정당화됐고([`lib.rs:23-36`](../../crates/rivet-tui/src/lib.rs)) 사용자 입력에는 그 논증이 적용되지 않는다. **채택한 `rebuild_transcript`와 무엇이 다른가**(§2.1-e 아래 문단이 정면으로 답한다): 로컬 에코는 **키 입력**을 출처로 삼아 fold의 출력에 **끼워 넣고**(순서·중복 규칙이 필요하다), 이쪽은 **로그**를 출처로 삼아 turn 경계에서 **통째로 갈아 끼운다**(전체 함수라 규칙이 없다). 전자는 화면이 두 번째 진실 원천이 되는 것이고, 후자는 화면이 유일한 진실 원천에 다시 맞추는 것이다. |
| **여러 turn의 종료 코드를 "가장 나쁜 것이 이긴다"로 정한다** | turn 1의 한도가 대화 전체를 실패로 만든다. 사용자는 그것을 보고 다른 것을 물어 정상적으로 끝냈다. 그리고 3·4·130 사이에 자연스러운 순서가 없다. |
| **`rivet resume --tui`가 오늘처럼 완결된 세션을 거절한다** | 4b 이후 **대화가 정상적으로 끝난 세션이 정확히 그 모양**이다(마지막이 tool call 없는 assistant, [`session_recovery.rs:306-310`](../../crates/rivet-runtime/src/session_recovery.rs)). 그러면 `resume --tui`가 가장 흔한 경우에 쓸모없다. 거절의 근거였던 "새 프롬프트를 받지 않는다"를 `--tui`가 없앤다. |

---

## 3. 구체적 변경

### 3.1 `crates/rivet-core` — 변이 하나, 필드 하나, doc 셋 (4b.3의 계약 절반)

- **`src/event.rs`** — `AgentEvent`에 변이 하나:
  ```rust
  /// A prompt the user submitted, as an observation.
  ///
  /// The durable twin is [`crate::session::SessionEvent::UserMessage`], written by
  /// `AgentLoop::run` at the same point. Whole rather than a fragment, so unlike
  /// [`AgentEvent::TextDelta`] this one *is* stored — under another name, in the log.
  InputReceived { text: String },
  ```
  `RunStarted` **바로 다음**에 놓는다. 선언 순서가 `one_of_each` → `Event::one_of_each` →
  `every_bus_topic_is_claimed`의 목록 순서를 정하므로([`lib.rs:79-90`](../../crates/rivet-core/src/lib.rs)),
  선언 순서를 발행 순서(run.started → input.received → turn.started)와 맞추면 그 목록이
  수명 순서로 읽힌다. 토픽은 `"agent.input.received"`
  ([`event.rs:172-183`](../../crates/rivet-core/src/event.rs)에 한 줄), 표본은
  [`event.rs:194-225`](../../crates/rivet-core/src/event.rs)에 한 줄.
- **`src/event.rs`** — `AgentEvent::RequestStarted`에 `dropped_turns: u32`
  ([`event.rs:130-133`](../../crates/rivet-core/src/event.rs)). 토픽은 늘지 않으므로 프로파일
  목록도 트립와이어 둘도 움직이지 않는다 — 둘 다 `{ .. }`로 맞춘다. **같은 파일의 표본
  리터럴도 필드를 얻는다** ([`event.rs:202-205`](../../crates/rivet-core/src/event.rs)의
  `one_of_each` 팔에 `dropped_turns: 0`) — `bus.rs:542`의 리터럴과 같은 종류이고, 그쪽은
  §6.0이 이름으로 짚는다.
- **`src/context.rs`** — `AssembledContext`에 `dropped_turns: u32`
  ([`context.rs:147-154`](../../crates/rivet-core/src/context.rs)). `dropped`의 doc "so a UI can
  warn about both"가 이 Phase에 처음으로 참이 되므로, **어느 필드가 어느 절반인지** 적는다:
  `dropped`는 컨텍스트 항목, `dropped_turns`는 잘린 대화 turn 수.
- **`src/session.rs`** — `SessionEvent::UserMessage`의 doc에 한 줄
  ([`session.rs:44-45`](../../crates/rivet-core/src/session.rs)): 관찰 쪽 쌍둥이가 생겼고 그 이름이
  `agent.input.received`라는 것. 계약은 안 바뀐다(이 변이에는 `run_id`가 없고, 그대로 둔다 —
  버스 쪽은 envelope이 `for_run`으로 상관관계를 나른다,
  [`event.rs:46-51`](../../crates/rivet-core/src/event.rs)).

`Message`·`Role`·`ContentBlock`은 그대로 쓴다. `Message::text()`
([`model.rs:199`](../../crates/rivet-core/src/model.rs))이 이미 있으므로 표시용 문자열을 뽑는
새 함수도 필요 없다.

### 3.2 `crates/rivet-runtime` — 발행 지점 하나와 `dropped_turns` 배선 (4b.3의 런타임 절반)

- **`src/agent_loop.rs`** — 발행 한 줄. `run()`이 `AgentEvent::RunStarted`를 발행한 **직후**
  ([`agent_loop.rs:207-213`](../../crates/rivet-runtime/src/agent_loop.rs) 다음)에
  `AgentEvent::InputReceived { text }`를 발행한다. `input`은
  [`:199`](../../crates/rivet-runtime/src/agent_loop.rs)에서 move되므로 그 앞에서
  `let echo = input.as_ref().map(Message::text);`로 집어 둔다.

  **순서가 결정이다.** durable append([`:199-206`](../../crates/rivet-runtime/src/agent_loop.rs))
  **뒤**여야 하는 이유는 자명하다(로그에 못 적은 것을 관찰로 광고하지 않는다). `RunStarted`
  **뒤**여야 하는 이유는 화면 쪽에 있다: `AppState`의 `RunStarted` 팔이 turn 경계에서 run 단위
  필드를 지우므로(§3.3), 입력이 먼저 도착하면 방금 그린 turn을 지우는 순서가 된다. 지금은
  지우는 대상에 transcript가 없어 무해하지만, 무해함에 기대는 순서를 남기지 않는다.
- **`src/agent_loop.rs`** — `build_request`가 `dropped_turns`를 밖으로 낸다.
  [`:413-425`](../../crates/rivet-runtime/src/agent_loop.rs)에서 받은 `assembled`의 그 필드가
  [`:427-434`](../../crates/rivet-runtime/src/agent_loop.rs)에서 버려지고 있다. 반환형을
  `Result<Result<Assembled, StopReason>>`로 바꾸고(`struct Assembled { request: ModelRequest,
  dropped_turns: u32 }`, 비공개), `turns`([`:291-294`](../../crates/rivet-runtime/src/agent_loop.rs))가
  `request`로 넘기고, `request`의 발행 지점
  ([`:476-481`](../../crates/rivet-runtime/src/agent_loop.rs))이 그것을 싣는다. 재시도마다 같은
  값이 다시 나가는데, 같은 조립을 다시 보내는 것이므로 맞다.
- **`src/context/mod.rs`** — `assemble`이 `dropped_turns`를 채우고
  ([`:178-183`](../../crates/rivet-runtime/src/context/mod.rs)), `dropped_report`
  ([`:221-231`](../../crates/rivet-runtime/src/context/mod.rs))는 합성 항목을 만들지 않고
  사라진다 — 호출 지점 한 곳뿐이다. `history::trim`
  ([`history.rs:72`](../../crates/rivet-runtime/src/context/history.rs))은 손대지 않는다.
  **형이 다르므로 변환을 여기 적어 둔다**: `Trimmed::dropped_groups`는 `usize`이고
  ([`history.rs:59`](../../crates/rivet-runtime/src/context/history.rs)) 새 필드는 `u32`다.
  워크스페이스가 `clippy::pedantic`을 켜 두었고([`Cargo.toml:85`](../../Cargo.toml)) 게이트가
  `-D warnings`이므로 맨 `as`는 `cast_possible_truncation`으로 막힌다. 쓰는 것은
  `u32::try_from(trimmed.dropped_groups).unwrap_or(u32::MAX)`이다 — 42억 turn을 버린 대화는
  없고, 있더라도 "아주 많다"가 맞는 답이다.
- **`src/session_recovery.rs`** — `ResumePlan`에 변이 하나:
  ```rust
  pub enum ResumePlan {
      Continue,
      /// Nothing to re-ask, but the log is open: a host that can take new input may.
      ///
      /// `rivet resume` alone cannot — it takes no prompt — so the CLI prints this and
      /// stops. `rivet resume --tui` has a composer, so it opens on one.
      AskForMore(String),
      Refuse(String),
  }
  ```
  `resume_plan`의 마지막 팔([`:306-310`](../../crates/rivet-runtime/src/session_recovery.rs))이
  `Refuse` 대신 `AskForMore`를 돌려준다. `closed`([`:283-289`](../../crates/rivet-runtime/src/session_recovery.rs))와
  빈 세션([`:291-295`](../../crates/rivet-runtime/src/session_recovery.rs))은 `Refuse` 그대로다 —
  전자는 로그에 덧붙이면 안 되고, 후자는 이어 갈 것 자체가 없다.
- `close_interrupted`([`:161`](../../crates/rivet-runtime/src/session_recovery.rs))·
  `read_all`([`:249`](../../crates/rivet-runtime/src/session_recovery.rs))·`inspect`·`Rli`는
  **한 줄도 바뀌지 않는다.** 이 Phase가 하는 일은 그것들을 turn마다 부르는 것이다.

### 3.3 `crates/rivet-tui` — 컴포저·모드·transcript (4b.1 · 4b.2 · 4b.5)

- **`src/app.rs`**
  - `Intent`는 그대로 둔다([`app.rs:30-37`](../../crates/rivet-tui/src/app.rs)) — `Quit`·`Cancel`,
    `Copy` 그대로. 새 타입 `Prompt(pub String)`이 둘째 채널을 탄다 (§2.1-c, §4.2).
  - `Tui::new() -> (Self, Intents)`. `Intents { control, prompts }` (§4.2). `Tui`는
    `prompts: Mutex<Option<mpsc::Sender<Prompt>>>`를 든다.
  - `dismiss()`([`:96-98`](../../crates/rivet-tui/src/app.rs))가 토큰을 취소하면서 **송신단을
    떨어뜨린다.** 멱등이고 한 방향인 성질은 그대로다.
  - `handle_key`([`:229-265`](../../crates/rivet-tui/src/app.rs))가 모드에 따라 갈라진다 (§4.4).
    `answer_approval` 우선권([`:239-241`](../../crates/rivet-tui/src/app.rs))은 남고, 그 앞의
    "맨 `c`는 모달이 안 가져간다" 예외가 **수식자를 보게** 바뀐다.
  - `request`([`:307-340`](../../crates/rivet-tui/src/app.rs))가 모달을 올리는 잠금 구간
    ([`:311-323`](../../crates/rivet-tui/src/app.rs))에서 컴포저를 일시정지시키고, 내리는 구간
    ([`:333-338`](../../crates/rivet-tui/src/app.rs))에서 되돌린다. 두 잠금을 함께, `state` 먼저
    잡는 순서는 그대로다.
  - `sync_transcript(&self, messages: &[Message])` — 새 공개 메서드 (§4.2).
- **`src/state.rs`**
  - `AppState`에 `transcript: Transcript`, `composer: Composer`. `RunView`에서 `text`·
    `text_dropped`가 빠지고 `out_of_context: u32`가 들어온다 (§4.2).
  - `apply`의 `Agent` 팔([`:183-214`](../../crates/rivet-tui/src/state.rs))에 두 줄:
    `InputReceived`는 `transcript.push_user(text)`, `RequestStarted`는 `model`과 함께
    `dropped_turns`를 읽어 `run.out_of_context`에 넣는다.
  - `RunStarted` 팔([`:184-187`](../../crates/rivet-tui/src/state.rs))이 `stop`·`retries`·
    `last_error`·`out_of_context`를 지운다. **`tools`는 지우지 않는다** — call id는 run마다
    고유하므로 충돌하지 않고, 사용자가 앞 turn에 무슨 도구가 돌았는지 보는 것이 옳다.
    `TOOL_LIMIT`(200, [`:26-27`](../../crates/rivet-tui/src/state.rs))의 범위가 run에서 대화로
    넓어진다는 것을 doc에 적는다; 넘치면 `tools_dropped`가 이미 말한다.
  - `push_text`([`:345-364`](../../crates/rivet-tui/src/state.rs))가 `Transcript::push_agent`가
    된다. 바이트로 자르고 문자로 세는 몸통은 그대로 옮긴다.
  - `AppState::rebuild_transcript(&[Message])` — 전체 함수 하나. 로그가 나른 메시지에서
    transcript를 **통째로 다시 만든다** (§2.1의 보수 경로, §4.2).
- **`src/draw.rs`**
  - 레이아웃이 세 구역에서 네 구역이 된다([`:26-33`](../../crates/rivet-tui/src/draw.rs)):
    패널 행 · **컴포저 한 줄** · 상태바 한 줄. 컴포저 줄은 모드와 무관하게 **항상** 한 줄이다 —
    모드에 따라 높이가 바뀌면 타이핑을 시작할 때 화면이 튄다. 새 `draw_composer`가 그 줄을
    그린다: `Watching`이면 흐린 `press [enter] to type`, `Composing`이면 `> ` 프롬프트와
    초안(§4.4). 80×24에서 패널이 한 줄을 내주는 것 말고는 아무것도 안 움직인다 — 상태바는
    여전히 마지막 줄이다.
  - `draw_agent`([`:143-202`](../../crates/rivet-tui/src/draw.rs))가 `run.text` 대신
    transcript를 그린다. `"… N character(s) elided"`
    ([`:150-155`](../../crates/rivet-tui/src/draw.rs))는 **같은 문장 그대로** 남고 출처만
    `transcript.dropped_chars`가 된다. `run.out_of_context > 0`이면 한 줄 더:
    `"… {n} earlier turn(s) are no longer sent to the model"`.
  - `status_line`([`:226-267`](../../crates/rivet-tui/src/draw.rs))의 힌트 세그먼트
    ([`:264`](../../crates/rivet-tui/src/draw.rs))가 모드를 따른다 — **문자열만 바뀌고 rank도
    개수도 그대로다.** `Watching`의 문자열은 오늘과 동일하고 `Composing`의 것은 그보다 짧다;
    §4.4가 왜 그 제약이 협상 불가인지(80칼럼에서 기존 단언 셋이 걸린다) 산수로 적었다.
- **`Cargo.toml`은 바뀌지 않는다.** 새로 필요한 것이 `rivet_core::model::Message` 하나이고
  `rivet-core`는 이미 있다 — `the_tui_crate_does_not_depend_on_the_runtime`
  ([`independence.rs:17`](../../crates/rivet-tui/tests/independence.rs))이 보는 성질은 그대로다.

### 3.4 `crates/rivet-cli` — turn 루프와 그 주변 (4b.6 · 4b.7 · 4b.4의 절반)

- **`src/run.rs`** — 이 Phase의 무게중심이다.
  - `Watching.intents`가 `Option<Intents>`가 된다([`:103`](../../crates/rivet-cli/src/run.rs)).
    `observe`([`:137-168`](../../crates/rivet-cli/src/run.rs))는 `Tui::new()`의 반환 모양만
    따라간다.
  - `started`([`:60-93`](../../crates/rivet-cli/src/run.rs))의
    `SessionState::replay(&store.read(session_id, 1, 1_000).await?)`
    ([`:81`](../../crates/rivet-cli/src/run.rs))가 **삭제된다.** 상태는 turn마다 `drive`가
    만든다. `resumed`([`:210-224`](../../crates/rivet-cli/src/run.rs))도 `state`를 안 넘긴다.
    `drive`의 인자에서 `state`가 빠지고 `input: Option<Message>`가 `FirstTurn`이 된다
    ([`:258-267`](../../crates/rivet-cli/src/run.rs)). `:257`의 `too_many_arguments` allow가
    아직 필요한지는 구현이 셈해서 정한다 — 필요 없어지면 지운다.
  - **`converse`의 `Option<RunSummary>`는 `started`에서 풀린다.** `drive`는
    `Result<Option<RunSummary>>`를 그대로 올려 보내고(turn이 0회일 수 있는 것은
    `FirstTurn::Ask`뿐이다), `start` 쪽 진입점인 `started`
    ([`:60-93`](../../crates/rivet-cli/src/run.rs))가 그것을 `Result<RunSummary>`로 좁힌다:
    `FirstTurn::Prompt`는 프롬프트 채널을 **읽기도 전에** 한 turn을 돌리므로 `None`이 불가능하고,
    그 불가능을 `expect`가 아니라 불변식을 말하는 `Error::internal`로 적는다. 그래야
    `start`·`finish`([`main.rs:330-335`](../../crates/rivet-cli/src/main.rs))의 시그니처가
    안 바뀐다. `resume` 쪽은 `Resumed`가 세 경우를 이름으로 가르므로 좁힐 것이 없다.
  - `drive`가 **설정 + teardown**만 하고, 반복은 새 함수 `converse`가 한다 — `start`/`started`
    ([`:52-56`](../../crates/rivet-cli/src/run.rs))와 `resume`/`resumed`
    ([`:202-205`](../../crates/rivet-cli/src/run.rs))가 이미 쓰는 갈라짐과 같은 이유다:
    루프 안의 `?`가 teardown 여섯 줄을 건너뛰면 안 된다.
  - 새 타입 셋: `CurrentTurn`(§2.1-a, §4.3), `FirstTurn`(§4.3), `Restored`(§4.3).
  - `Screen::start`([`:433-437`](../../crates/rivet-cli/src/run.rs))가 `&CancellationToken`
    대신 `&CurrentTurn`을 받고, 펌프([`:452-463`](../../crates/rivet-cli/src/run.rs))의 지역
    변수 `asked_to_cancel`([`:456`](../../crates/rivet-cli/src/run.rs))이 **사라진다** —
    `CurrentTurn`이 들고 있다.
  - `Screen::stop`([`:480-488`](../../crates/rivet-cli/src/run.rs))이 `self`를 소비하고
    `Restored`를 돌려준다. `Drop`([`:491-510`](../../crates/rivet-cli/src/run.rs))은 그대로 —
    `stop` 뒤에는 두 핸들이 `None`이라 아무것도 안 한다.
  - `Reaction`([`:518-526`](../../crates/rivet-cli/src/run.rs))과 `Reaction::to`
    ([`:530`](../../crates/rivet-cli/src/run.rs))·`carry_out`
    ([`:548`](../../crates/rivet-cli/src/run.rs))은 **바뀌지 않는다.** `CurrentTurn::react`가
    그것들을 부른다.
  - `answer`([`:380-400`](../../crates/rivet-cli/src/run.rs))가 **삭제된다.** 호출 지점
    ([`:356-358`](../../crates/rivet-cli/src/run.rs))과 "the first N character(s) scrolled out of
    the panel" 사과도 함께 간다.
  - `report`([`:583-594`](../../crates/rivet-cli/src/run.rs))가 `&Restored`를 첫 인자로 받는다.
    호출 지점([`:363`](../../crates/rivet-cli/src/run.rs))은 **오늘도 이미** `screen.stop()`
    ([`:344-346`](../../crates/rivet-cli/src/run.rs)) 뒤에 있다. 바뀌는 것은 그것이 **루프 밖
    teardown으로 나가는 것**이고, 그러면서 그 순서가 줄 순서로 지켜지던 것에서 **타입이 강제하는
    것**이 된다 (§4.3).
  - `resume`([`:196-207`](../../crates/rivet-cli/src/run.rs))이 `ResumePlan`의 세 변이를
    가른다. 반환형이 `Result<Option<RunSummary>>`에서 `Result<Resumed>`로 (§4.3).
- **`src/main.rs`** — `finish`([`:330-335`](../../crates/rivet-cli/src/main.rs))는 그대로
  `Result<RunSummary>`를 받는다 — `started`가 `Option`을 이미 좁혔다(위).
  `Resume` 갈래([`:281-288`](../../crates/rivet-cli/src/main.rs))가 `Resumed` 셋을 가른다:
  `Ran → exit::for_stop`, `Refused → exit::CONFIG`, `NothingAsked → exit::OK`.
  `--tui`의 도움말([`:47-52`](../../crates/rivet-cli/src/main.rs))에 대화형이라는 것과 `q`가
  대화를 끝낸다는 것.
- **`src/render/human.rs`** — 팔 하나를 **명시적으로** 더한다
  ([`:86`](../../crates/rivet-cli/src/render/human.rs)의 `_ => {}` 앞):
  ```rust
  // Not echoed. In this mode the prompt came from argv and is already on the user's
  // screen; printing it back is noise. Explicit rather than left to `_ => {}` so the
  // next reader sees a decision instead of an omission.
  Event::Agent(AgentEvent::InputReceived { .. }) => {}
  ```
- **`src/render/jsonl.rs`** — **한 줄도 안 바뀐다.** 그것이 4b.4가 답해야 할 질문이다 (§4.5).
- **`src/config.rs`** — `subscribable_topics`
  ([`:333`](../../crates/rivet-cli/src/config.rs))의 목록은 **안 바뀐다.**
  `every_topic_is_granted_or_deliberately_withheld`
  ([`:680`](../../crates/rivet-cli/src/config.rs))의 `should_reach_a_narrowed_profile`에
  `AgentEvent::InputReceived { .. } => false` 팔이 붙고, 그 옆 주석이 이유를 적는다.

### 3.5 `plugins/telemetry-log` — 대화가 둘이 된다 (4b.4)

- **`src/lib.rs`**
  - `CONVERSATION_TOPIC`([`:73`](../../plugins/telemetry-log/src/lib.rs))이
    `CONVERSATION_TOPICS: [&str; 2] = ["agent.text.", "agent.input."]`로 **대체된다**(별칭 없음).
  - `DEFAULT_TOPICS`([`:63-70`](../../plugins/telemetry-log/src/lib.rs))는 **안 바뀐다** —
    `agent.input.`은 이미 없다. 그 doc([`:57-59`](../../plugins/telemetry-log/src/lib.rs))의
    "`agent.text.`가 absent"가 "둘 다 absent"가 된다.
  - 게이트([`:176-192`](../../plugins/telemetry-log/src/lib.rs))가 두 접두사를 다 본다. 오류
    문구는 **어느 절반에 닿았는지** 댄다 — "which reaches `agent.input.`"과
    "which reaches `agent.text.`"는 운영자가 고칠 방법이 다르지 않지만, 무엇을 잘못 적었는지는
    다르다.
  - 밀어 넣기([`:193-198`](../../plugins/telemetry-log/src/lib.rs))가 둘을 각각 검사해 각각
    넣는다. `covers_whole`([`capability.rs:241`](../../crates/rivet-core/src/capability.rs))은
    접두사 하나에 대한 질문이므로 두 번 부른다.
  - `Settings.promised`의 doc([`:93-100`](../../plugins/telemetry-log/src/lib.rs))에서
    `CONVERSATION_TOPIC` 링크가 복수형이 된다.
- **`src/record.rs`** — `agent_detail`([`:117-163`](../../plugins/telemetry-log/src/record.rs))에
  팔 하나:
  ```rust
  // Length only, exactly like `TextDelta` above and for the same reason: the text is
  // what `include_conversation` gates, and a record carrying it would put the user's
  // half of the conversation in the log through the back door.
  AgentEvent::InputReceived { text } => Detail {
      count: Some(text.len() as u64),
      ..Detail::default()
  },
  ```
  `RequestStarted` 팔([`:127-134`](../../plugins/telemetry-log/src/record.rs))은 새 필드 때문에
  컴파일이 깨진다 — `..`를 더해 무시한다. `Detail`은 세 필드이고
  ([`record.rs:86-99`](../../plugins/telemetry-log/src/record.rs)) 그 셋을 늘리는 것은 이
  Phase의 일이 아니다.
- **`rivet-plugin.toml`은 안 바뀐다** — `events_subscribe`를 scope 없이 요청하고 프로파일이
  좁힌다.

### 3.6 문서·예제 (4b.8)

**계획서의 [`### 4b.8` 표](../plan.md#4b8--이-phase가-끝나는-날-거짓이-되는-문장)가 그 목록이고,
여기서 다시 만들지 않는다.** 열다섯 행 전부가 이 설계의 결정과 일치한다 — 이 설계는 토픽을
하나 더하고(행 1·2·3·4·5·6), `CONVERSATION_TOPIC`을 둘로 만들고(행 7·8·9·10·13), 화면을
대화형으로 만들고(행 12·14·15), 한 번의 실행을 run 하나가 아니게 만든다(행 11).

§8-6이 그 표에 **세 행을 보탠다** — 이 설계가 계획서가 예상하지 않은 두 가지(`RequestStarted`의
새 필드, `resume --tui`의 새 뜻)를 하기 때문이다.

`rivet.example.toml`을 읽는 테스트가 셋이고(`the_shipped_example_loads`
[`config.rs:800`](../../crates/rivet-cli/src/config.rs), `the_shipped_example_keeps_its_named_agent`
[`config.rs:854`](../../crates/rivet-cli/src/config.rs), `every_id_the_shipped_example_enables_resolves`
[`catalog.rs:365`](../../crates/rivet-cli/src/catalog.rs)) **셋 다 그대로 통과한다** — 4b.8이 그
파일에서 고치는 것은 `:92-93`·`:97-100`의 **주석**뿐이고, 셋은 전부 TOML을 파싱해 값을 본다
(§6.0 ③).

---

## 4. 데이터·인터페이스 모양

### 4.1 새 토픽 하나, 새 필드 둘

```rust
// crates/rivet-core/src/event.rs
pub enum AgentEvent {
    RunStarted { agent_id: AgentId, model: ModelId },
    /// A prompt the user submitted, as an observation.
    ///
    /// The durable twin is `SessionEvent::UserMessage`. Whole rather than a fragment, so
    /// `docs/events.md`'s `← 저장 안 됨` marker does **not** apply: that marker means "this
    /// is a piece, and pieces are not stored", which is why only `agent.text.delta` and
    /// `tool.execute.progress` carry it.
    InputReceived { text: String },
    TurnStarted { turn: u32 },
    RequestStarted {
        model: ModelId,
        input_tokens_estimate: u64,
        /// History turns the assembler dropped to make this request fit.
        ///
        /// A number rather than a flag: a UI showing a transcript the model can no longer
        /// see has to say *how much* of it. Cumulative within a run, because the array it
        /// counts against grows every turn.
        dropped_turns: u32,
    },
    // … 나머지는 그대로
}
```

토픽: `"agent.input.received"`. 표본:
`Self::InputReceived { .. } => Self::InputReceived { text: String::new() }`.

```rust
// crates/rivet-core/src/context.rs
pub struct AssembledContext {
    pub system: String,
    pub messages: Vec<Message>,
    /// Context **items** dropped for budget.
    pub dropped: Vec<DroppedItem>,
    /// History **turns** dropped for budget.
    ///
    /// Split from `dropped` rather than encoded in one of its keys. It used to arrive as
    /// `DroppedItem { key: "history.turns:3" }`, which is a number a UI would have had to
    /// parse out of a string — so no UI ever did, and the doc above promising one could
    /// "warn about both" was true only of the half that had a field.
    pub dropped_turns: u32,
    pub estimated_tokens: u32,
}
```

### 4.2 `rivet-tui`의 공개 API

```rust
/// A prompt the user composed and submitted.
///
/// A type of its own rather than a third `Intent`, because the two travel on channels with
/// opposite disciplines: an intent is idempotent and may be dropped when the host is behind,
/// a prompt is unique content and may not. Splitting the types rather than the send makes
/// "these two got crossed" impossible to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt(pub String);

/// What a screen sends the host.
#[derive(Debug)]
pub struct Intents {
    /// `Quit` and `Cancel`. Bounded and lossy, and the reason is unchanged: a full channel
    /// means the host is not reading, blocking here would stop the redraw the user is
    /// waiting on, and the duplicate being dropped is a duplicate of something idempotent.
    pub control: mpsc::Receiver<Intent>,
    /// Submitted prompts. Capacity one, and never dropped: the host takes one at the top of
    /// every turn, so the slot is free whenever a turn is running. A submit that finds it
    /// full leaves the draft **in the composer**, where the user can see it — which is the
    /// one thing `try_send` on the control channel does not do.
    ///
    /// `None` from `recv()` means the screen is gone: `Tui::dismiss` drops the sender, so
    /// "no screen" and "no more prompts" are one fact rather than two that can disagree.
    pub prompts: mpsc::Receiver<Prompt>,
}

/// Which panel has focus.  (unchanged)
pub enum Panel { Agent, Jobs }

/// Whether keys are commands or text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputMode {
    #[default]
    Watching,
    Composing,
}

/// The input line.
///
/// Input state, not an observation — the same kind of thing as `AppState::focus`, which a
/// key press has written since Phase 3. It is **not** the second exception `AppState::pending`
/// is: `pending` is written by the `ApprovalSink` because an approval is a round trip with
/// nowhere else to put its answer. This is just what the user has typed.
#[derive(Clone, Debug, Default)]
pub struct Composer {
    pub mode: InputMode,
    /// Typed and not yet submitted. Kept across a suspension and across a failed handover.
    pub draft: String,
    /// The last submit could not be handed over. Cleared by the next key.
    pub unsent: bool,
    /// Mode to return to when an approval modal goes away.
    suspended: Option<InputMode>,
}

/// The conversation as the screen has seen it, oldest first.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    /// Characters dropped off the front. **Characters**, not bytes: the cap is in bytes and
    /// a CJK character is three of them, so counting bytes told a reader of Korean output
    /// that three times as much went missing as did.
    pub dropped_chars: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// From `agent.input.received`.
    User(String),
    /// Accumulated from `agent.text.delta`.
    Agent(String),
}

/// How many **bytes** of conversation the panel keeps.
///
/// Eight times the old `TEXT_LIMIT`, because the buffer's scope went from a run to a
/// conversation. Still bounded, and still says what it elided.
const TRANSCRIPT_LIMIT: usize = 65_536;
```

`AppState`:

```rust
pub struct AppState {
    pub run: RunView,          // stop · retries · last_error · turn · tools · out_of_context
    pub transcript: Transcript,
    pub composer: Composer,
    pub jobs: JobView,
    pub status: StatusView,
    pub focus: Panel,
    pub pending: Option<ApprovalView>,
}

impl AppState {
    /// Rebuild the transcript from the durable log.
    ///
    /// **The one repair the bus cannot make for itself.** `upsert_tool` can raise a tool line
    /// from its completion when `tool.requested` was dropped, because a later event on the
    /// same call arrives. A dropped `agent.input.received` has no later event: one lost
    /// envelope is one user turn missing from the screen, permanently.
    ///
    /// So the host hands the log back at every turn boundary — it has just read it to the end
    /// anyway — and this replaces the transcript wholesale. **Replaces, not weaves**: a total
    /// function of the messages, so there is no alignment or dedup rule to get wrong, and it
    /// is idempotent. Called only between turns, when nothing is streaming.
    ///
    /// `Role::Tool` and `Role::System` are skipped: tool calls are `run.tools`, and the
    /// system prompt is not conversation.
    pub fn rebuild_transcript(&mut self, messages: &[Message]);
}
```

`Tui`:

```rust
impl Tui {
    #[must_use]
    pub fn new() -> (Self, Intents);

    /// The screen is gone: stop drawing, stop being an approval sink, **and stop being a
    /// place prompts can come from.** One-way and idempotent.
    pub fn dismiss(&self);

    /// Make the transcript agree with the log. See `AppState::rebuild_transcript`.
    pub fn sync_transcript(&self, messages: &[Message]);
}
```

### 4.3 `rivet-cli`의 turn 루프

```rust
/// The one thing the pump sees that changes between turns.
///
/// One value rather than two, because the two facts have to change together. The token a
/// turn watches and whether that turn has already been asked to stop are reset by the same
/// call; reset one without the other and either the second Ctrl-C of turn 2 kills the
/// process (`asked` stale) or the first one cancels a token nothing is watching (`cancel`
/// stale). The second is worse: the run does not stop at all.
///
/// It lives here, in the host, and not in `rivet-tui`: the TUI holds no token by design —
/// "it says somebody asked to [cancel]; the host owns the token and decides what cancel
/// means" (`app.rs:8-10`).
#[derive(Clone, Debug, Default)]
struct CurrentTurn(Arc<Mutex<TurnState>>);

#[derive(Debug, Default)]
struct TurnState {
    cancel: CancellationToken,
    asked: bool,
}

impl CurrentTurn {
    /// Start a turn. A fresh token and a fresh two-step budget, under one lock.
    ///
    /// Fresh rather than a child of a long-lived parent: `CancellationToken` is a latch and
    /// `Watchdog::start` takes `cfg.cancel.child_token()` (`agent_loop.rs:762`), so a child
    /// of a cancelled parent is born cancelled. And the parent cannot be the thing the pump
    /// holds, because the pump is what cancels.
    fn begin(&self) -> CancellationToken;

    /// The pump's whole decision, against the turn as it stands **now**.
    ///
    /// Returns the token as well as the reaction so the caller cancels the one this turn is
    /// watching rather than one it captured when it was spawned. `Reaction::to` stays a pure
    /// function of `(intent, &mut asked)` and its unit test stays green — which is exactly
    /// why the regression needs a test *here*: `a_second_cancel_intent_forces_the_exit`
    /// (`run.rs:602`) uses its own local `asked` and cannot see a stale one.
    fn react(&self, intent: Intent) -> (Reaction, CancellationToken);
}

/// How the conversation starts.
enum FirstTurn {
    /// `rivet "…"` — run this prompt.
    Prompt(Message),
    /// `rivet resume` on a session with somewhere to go — run with the log as it stands.
    Log,
    /// `rivet resume --tui` on a session that has already answered. There is nothing to
    /// re-ask, and now there is somewhere to type: the screen opens on the composer.
    Ask,
}

/// One turn, as the loop needs it.
///
/// A trait with one method rather than a call to `AgentLoop::run` written inline, so the
/// *shape* of the loop — what repeats, what is asked for again, when it stops asking — is
/// testable with no model, no plugin and no terminal. `LiveTurns` is the only production
/// implementation, and what it does per turn is the answer to "what goes inside the loop":
/// `CurrentTurn::begin`, `signals::install`, a fresh `RunConfig`, `AgentLoop::run`,
/// `signals.abort`.
#[async_trait]
trait Turns {
    async fn run(
        &mut self,
        session_id: SessionId,
        state: SessionState,
        input: Option<Message>,
    ) -> rivet_core::Result<RunSummary>;
}

/// Proof that the alternate screen is gone.
///
/// `Screen::stop` is the only thing that makes one from a screen, and `report` demands one.
/// So "the summary is printed after the terminal comes back" is a property of the types
/// rather than of the order two statements happen to be written in — which matters here
/// because `stop` **awaits** the render task to get that ordering (`run.rs:472-478`) and
/// `Drop` cannot await, so a loop that printed inside would race the screen going away.
#[derive(Debug)]
struct Restored;

impl Restored {
    /// `--human` and `--jsonl`: there was no alternate screen to give back.
    fn nothing_to_restore() -> Self;
}

/// How a `rivet resume` ended.
pub enum Resumed {
    /// Turns ran. The **last** one decides the exit code.
    Ran(RunSummary),
    /// The log had nowhere to go and nothing here could add to it.
    Refused,
    /// A screen opened on the composer and the user quit without asking anything.
    NothingAsked,
}
```

그리고 루프 자체:

```rust
/// The turn loop: everything that repeats, and nothing that does not.
///
/// Split out of `drive` for the reason `started` is split out of `start` (`run.rs:52-56`):
/// the teardown after it has to run on the failing path too, and a `?` inside a loop cannot
/// be followed by six lines of shutdown.
async fn converse(
    turns: &mut dyn Turns,
    store: &Arc<JsonlSessionStore>,
    session_id: SessionId,
    first: FirstTurn,
    prompts: Option<&mut mpsc::Receiver<Prompt>>,
    tui: Option<&Arc<Tui>>,
) -> rivet_core::Result<Option<RunSummary>> {
    let mut last = None;
    let mut next = Some(first);
    loop {
        let input = match next.take() {
            Some(FirstTurn::Prompt(message)) => Some(message),
            Some(FirstTurn::Log) => None,
            // `Ask` on the first turn, and every turn after the first.
            Some(FirstTurn::Ask) | None => match prompts.as_deref_mut() {
                // A closed channel is a screen that is gone, which is the end of the
                // conversation -- `Tui::dismiss` drops the sender.
                Some(rx) => match rx.recv().await {
                    Some(Prompt(text)) => Some(Message::user(text)),
                    None => break,
                },
                // `--human` and `--jsonl` have no composer: one turn, exactly as today.
                None => break,
            },
        };

        // Repair, then read to the end. Both, every turn, and in that order -- `close_interrupted`
        // reads the whole log itself (`session_recovery.rs:165`, `:214`), so `read_all` is
        // never called from here: there is one path into the log rather than two.
        let state = session_recovery::close_interrupted(store.as_ref(), session_id).await?;
        if let Some(tui) = tui {
            tui.sync_transcript(&state.messages);
        }
        last = Some(turns.run(session_id, state, input).await?);
    }
    Ok(last)
}
```

**루프 밖에 서는 것과 그 이유.**

| 무엇 | 어디 | 왜 |
|---|---|---|
| `agent_spec` · `context_providers` · `ContextAssembler` ([`run.rs:268-272`](../../crates/rivet-cli/src/run.rs)) | 밖 | 설정과 레지스트리의 함수다. turn이 바꾸지 않는다 |
| `sink`([`run.rs:295-302`](../../crates/rivet-cli/src/run.rs))와 `unattended(..)`의 **값**([`:306`](../../crates/rivet-cli/src/run.rs)·[`:376-378`](../../crates/rivet-cli/src/run.rs)) | 밖 | 같은 `Arc<Tui>`가 대화 내내 승인을 받으므로 sink도 그 `bool`도 대화 단위다. **대입은 그렇지 않다**: `RunConfig`가 turn마다 새로 만들어지므로(아래) `cfg.unattended`·`cfg.approval_sink` 필드 대입은 turn마다 다시 일어난다. 한 번 계산하고, 매번 싣는다 |
| `Screen::start`([`run.rs:283-286`](../../crates/rivet-cli/src/run.rs)) | 밖 | 화면이 run보다 오래 사는 것이 이 Phase의 전부다 |
| `AgentLoop::new`([`run.rs:312-319`](../../crates/rivet-cli/src/run.rs)) | 밖 | 지터의 씨가 **대화 단위**가 된다. `RunConfig`가 루프 안이므로 루프 밖에는 아직 어떤 turn의 `run_id`도 없다 — 그래서 `FullJitter::for_run(cfg.run_id)`([`:318`](../../crates/rivet-cli/src/run.rs))가 **`FullJitter::default()`**가 된다. 잃는 것이 없다: `Default`의 몸통이 이미 `Self::for_run(RunId::new())`이고([`jitter.rs:57-60`](../../crates/rivet-runtime/src/jitter.rs)) 씨에는 벽시계 나노초가 섞이므로([`jitter.rs:35-42`](../../crates/rivet-runtime/src/jitter.rs)) `run_id`로 재현되는 백오프 같은 것은 애초에 없었다. 계약이 요구하는 성질 — "두 동시 run이 lockstep으로 재시도하지 않는다"([`jitter.rs:32`](../../crates/rivet-runtime/src/jitter.rs)) — 은 프로세스마다 다른 씨면 충족된다 |
| `watching.finish()`([`run.rs:337`](../../crates/rivet-cli/src/run.rs)) | 밖 | observer를 **가져가서** 드레인하고([`:118-126`](../../crates/rivet-cli/src/run.rs)), `Observer`의 `Drop`은 펌프를 abort한다([`:112`](../../crates/rivet-cli/src/run.rs)). 루프 안이면 화면이 turn 2부터 이벤트를 못 받는다 |
| `host.shutdown` · `shutting_down` · `unload_all`([`run.rs:323-332`](../../crates/rivet-cli/src/run.rs)) | 밖 | 런타임이 내려가는 것은 대화가 끝날 때 한 번이다 |
| `screen.stop()`([`run.rs:344-346`](../../crates/rivet-cli/src/run.rs)) | 밖 | DoD 1이 붙드는 것이 정확히 이 배치다 |
| `report`([`run.rs:363`](../../crates/rivet-cli/src/run.rs)) | 밖, `stop` 뒤 | `Restored`를 요구하므로 다른 자리에 놓을 수 없다 |
| `RunConfig`([`run.rs:304-310`](../../crates/rivet-cli/src/run.rs)) | **안** | `run()`에 move된다([`:321`](../../crates/rivet-cli/src/run.rs)). 어차피 turn마다 다시 만들어지고, `run_id`도 그때 새로 난다 |
| `signals::install`/`abort`([`run.rs:274`](../../crates/rivet-cli/src/run.rs)·[`:323`](../../crates/rivet-cli/src/run.rs)) | **안** | 둘 다 `drive` 안에 있으므로 통째로 들어가면 매 turn 그 turn의 토큰을 집는다. `signals.rs`는 안 바뀐다 |
| `CurrentTurn::begin` (이 절의 타입 정의) | **안** | 토큰과 `asked`가 함께 리셋된다 |
| `close_interrupted`([`session_recovery.rs:161`](../../crates/rivet-runtime/src/session_recovery.rs)) | **안** | 계획서의 결정: turn마다 수리한다. 건강한 세션에서는 `Rli::Satisfied`가 아무것도 안 쓰고 `replay`를 돌려준다([`:175`](../../crates/rivet-runtime/src/session_recovery.rs)) |
| `read_all`([`session_recovery.rs:249`](../../crates/rivet-runtime/src/session_recovery.rs)) | **안, 그러나 간접으로** | `close_interrupted`가 부른다. **turn 루프 안에서** 직접 부르지 않는 것이 요점이다 — 반복하는 자리에 문이 하나여야 `store.read(id, 1, 1_000)`이 다시 생겨나지 않는다. `resume`의 직접 호출([`run.rs:187`](../../crates/rivet-cli/src/run.rs))은 **남는다**: `check_workspace`와 `resume_plan`이 화면을 켜기도 전에 답해야 하는 질문이고, 루프에 들어가지 않으므로 반복되지 않는다 |

비용은 정직하게 적는다: turn마다 O(n) 읽기이므로 대화 전체로 O(n²)다. 사이에 모델 왕복이
있어 묻히겠지만, 묻힌다는 것과 없다는 것은 다르다. `READ_PAGE = 10_000`
([`session_recovery.rs:75`](../../crates/rivet-runtime/src/session_recovery.rs))이므로 만 이벤트까지는
페이지 하나다.

### 4.4 키 맵

| 키 | `Watching` | `Composing` | 승인 모달이 떠 있을 때 |
|---|---|---|---|
| `Ctrl-C` | `Intent::Cancel` | `Intent::Cancel` | `Intent::Cancel` — 모달이 절대 안 가져간다([`app.rs:236-241`](../../crates/rivet-tui/src/app.rs)) |
| 맨 `c` | `Intent::Cancel` (오늘 그대로, [`app.rs:243-247`](../../crates/rivet-tui/src/app.rs)) | 문자 `c` | 모드를 따른다 |
| `q` | `Intent::Quit` | 문자 `q` | 모드를 따른다 |
| `Esc` | `Intent::Quit` | 컴포저를 나간다 (**초안은 남는다**) | Deny (모달이 먼저 집는다) |
| `Tab` | 패널 포커스([`app.rs:249-256`](../../crates/rivet-tui/src/app.rs)) | 아무것도 아님 | 패널 포커스 |
| `Enter` | 컴포저로 들어간다 | 비었으면 나가고, 아니면 **제출하고** 나간다 | **무효** — 모달은 `Enter`를 답으로 쓰지 않고, 컴포저도 열지 않는다 (아래 3단계) |
| `y`·`n`·`a` | 아무것도 아님 | 문자 | 모달이 답한다([`app.rs:213-218`](../../crates/rivet-tui/src/app.rs)) |
| Backspace | 아무것도 아님 | 한 글자 지운다 | — |
| 그 밖의 인쇄 가능 문자 | 아무것도 아님 | 초안에 붙는다 | 삼켜지지 않는다(`_ => return false`, [`app.rs:217`](../../crates/rivet-tui/src/app.rs)) |

`handle_key`의 순서:

1. `KeyEventKind::Press`가 아니면 반환 ([`app.rs:233-235`](../../crates/rivet-tui/src/app.rs), 그대로)
2. **취소 키면 `Intent::Cancel`** — 모달도 컴포저도 안 가져간다. 취소 키의 정의만 바뀐다:
   `Ctrl-C`는 언제나, 맨 `c`는 `Watching`에서만. 오늘의 예외
   ([`app.rs:239`](../../crates/rivet-tui/src/app.rs))는 `Char('c')`를 수식자와 무관하게 뺐다
3. 모달이 있으면 `answer_approval` ([`app.rs:239-241`](../../crates/rivet-tui/src/app.rs), 그대로).
   모달이 있는 동안 모드는 언제나 `Watching`이다 — `request`가 컴포저를 재웠으므로,
   보이지 않는 상자에 타이핑이 들어가는 상태가 존재하지 않는다.
   **그 불변식을 지키려면 `Enter`가 여기서 멈춰야 한다**: `answer_approval`의 `match`는
   `y`·`a`·`n`·`Esc` 밖의 키를 `_ => return false`로 떨어뜨리므로
   ([`app.rs:213-218`](../../crates/rivet-tui/src/app.rs)) `Enter`는 그대로 4단계에 닿고
   모드를 `Composing`으로 뒤집는다 — 모달이 아직 화면을 덮고 있는 채로. 그래서 이 단계의
   규칙은 "모달이 답한 키면 끝"이 아니라 **"`state.pending`이 있으면 모드를 바꾸는 키는
   전부 무효"**다. 오늘 그 키는 `Enter` 하나뿐이다
4. 모드로 갈라진다

**모드 전환 키를 `Enter` 하나로 정한 이유.** `i`(vim식)나 `/`도 후보였다. `Enter`는 오늘
`Watching`에서 아무 뜻도 없고, "치고 → Enter → 답 → 치고" 흐름에서 **여는 키와 보내는 키가
같다**. 빈 초안에 `Enter`는 보내지 않고 나가기만 하므로, 실수로 두 번 눌러 빈 turn이 도는 일이
없다. 대가는 `Watching`에서 `Enter`를 잘못 누른 사용자가 `q`가 안 듣는 모드에 들어가는 것이고,
그건 **컴포저 줄이** 갚는다 — 상태바가 아니라. 이유는 다음 항목이다.

#### 모드 안내가 어디 있고, 왜 상태바가 아닌가

**상태바에는 자리가 없다. 이건 취향이 아니라 산수다.** `fit`
([`draw.rs:270-297`](../../crates/rivet-tui/src/draw.rs))은 rank 순으로 넣다가 넘치면
세그먼트를 **통째로** 버린다. 80칼럼에서 힌트(rank 5) 차례가 왔을 때 이미 쓴 폭은
`turn 2`(6) + `0↑/0↓ tok`(9+3) + `1 tool`(6+3) + `openai/gpt-4o`(13+3) = **43**이므로, 힌트에
남은 예산은 `80 − 43 − 3 = 34`자다. 오늘의 힌트
(`[q] quit  [c] cancel  [tab] panel`)는 **33자**라 딱 들어간다(합계 79).

그러므로 **`[enter] type`을 붙여 47자로 만들면 힌트가 통째로 버려진다**(`43+3+47 = 93 > 80`).
그리고 그 자리에 rank 6·7의 id 두 개가 대신 들어가므로 기존 단언 **셋**이 깨진다:

- `a_narrow_status_bar_keeps_the_segments_a_user_cannot_work_without`
  ([`draw.rs:304`](../../crates/rivet-tui/src/draw.rs))의 `contains("[q] quit")`
  ([`:312`](../../crates/rivet-tui/src/draw.rs))
- 같은 테스트의 `!narrow.contains(" pl")`([`:314`](../../crates/rivet-tui/src/draw.rs)) —
  힌트가 빠진 자리에 `0 plugin`이 들어가 `" pl"`이 생긴다. 이 단언은 "세그먼트가 단어 중간에서
  잘렸다"를 잡으려던 것인데 여기서는 **잘리지 않은 채로** 걸린다
- `the_three_panels_fit_an_eighty_column_terminal`
  ([`panels.rs:226`](../../crates/rivet-tui/tests/panels.rs))의
  `rows[23].contains("[q] quit")`([`:252`](../../crates/rivet-tui/tests/panels.rs))

**그래서 규칙을 하나 세운다: 어느 모드의 힌트도 오늘의 33자를 넘지 않는다.** 그 규칙 아래에서
힌트는 이렇게 된다.

| 모드 | 힌트 | 길이 | 80칼럼에서 |
|---|---|---|---|
| `Watching` | `[q] quit  [c] cancel  [tab] panel` | 33 | **오늘과 한 글자도 다르지 않다** — 그러므로 위 세 단언이 그대로 통과한다 |
| `Composing` | `[esc] leave  [enter] send` | 25 | 33보다 **짧으므로**, 오늘의 힌트가 살아남는 모든 폭에서 살아남는다 (실측: 합계 79) |

그리고 **모드 안내는 컴포저 줄이 한다.** 그 줄은 모드와 무관하게 항상 한 줄이고(§3.3), 안내가
말하는 대상이 바로 그 줄이므로 붙을 자리가 거기다:

- `Watching`: 흐린 안내 한 줄 — `press [enter] to type`
- `Composing`: `> ` 프롬프트와 초안과 커서. 초안이 있는데 `Esc`로 나온 상태면 `Watching`에서도
  흐리게 그대로 보인다(§7-7), 그래서 "내가 뭘 쓰다 말았지"가 화면에 남는다

`Ctrl-C`가 `Composing` 힌트에 없는 것은 의도다. 넣으면
(`[esc] leave  [enter] send  [ctrl-c] cancel`, 42자) 33자 규칙을 깨고 위 세 단언을 도로
깨뜨린다. Ctrl-C는 터미널에서 가장 보편적인 관습이고 `Watching` 힌트가 `[c] cancel`로 이미
광고한다 — 그리고 §5-9가 적은 대로 두 모드·모달 유무와 무관하게 동작한다.

(`the_status_bar_shows_only_what_events_carry`
[`panels.rs:201`](../../crates/rivet-tui/tests/panels.rs)가 금지하는 네 단어 —
`developer`·`readonly`·`workspace`·`profile` — 는 어느 쪽에도 없다.)

### 4.5 `--jsonl`과 `--human`은 새 토픽을 어떻게 다루는가 (4b.4)

**`--jsonl`: 그대로 내보낸다. 코드는 한 줄도 안 바뀐다.** `JsonlRenderer`는
`serde_json::to_string(envelope)` 한 줄이고 `AgentEvent` `match`가 없다
([`jsonl.rs:30-36`](../../crates/rivet-cli/src/render/jsonl.rs)). 새 토픽은 공짜로 실린다.
**문제는 공짜라는 것이므로, 명시적으로 답한다 — 그대로 두는 것이 옳다:**

1. 그 스트림은 이미 `agent.text.delta`를 **전문 그대로** 나른다. 사용자가 친 절반만 빼면
   스트림이 대화의 절반을 말하게 되고, 그건 관찰 스트림이 할 일이 아니다.
2. `--jsonl`은 `--tui`와 함께 쓸 수 없다([`main.rs:44-52`](../../crates/rivet-cli/src/main.rs)).
   그러므로 그 모드에서 프롬프트의 출처는 argv뿐이고, argv는 그 stdout을 읽는 바로 그 사람이
   방금 타이핑한 것이다. 새로 드러나는 것이 없다.
3. `JsonlRenderer`에 변이별 필터를 두면 그 파일에 처음으로 `AgentEvent` `match`가 생기고,
   스트림의 내용이 모드에 따라 달라진다 — 그 파일의 doc이 스스로 아니라고 적은 것이다
   ([`jsonl.rs:1-6`](../../crates/rivet-cli/src/render/jsonl.rs)).

**비대칭 하나를 적어 둔다**: `Profile::subscribable_topics`가 좁은 프로파일에서 이 토픽을
막는 것은 **plugin 구독자**에 대해서다 — `GuardedRegistry::register_subscriber`가 그것을
meet한다. 호스트 자신의 렌더러는 그 경로를 지나지 않고
([`render/mod.rs:68-87`](../../crates/rivet-cli/src/render/mod.rs)의 `bus.observe`), 지날 이유도
없다: 호스트의 stdout은 제3자가 아니라 운영자 자신이다.

**`--human`: 안 찍는다. 다만 `_ => {}`가 아니라 팔로 적는다** (§3.4). 이 모드에서 프롬프트는
argv에서 왔고 사용자의 스크롤백에 이미 있다. 되찍으면 중복이다. 명시적인 팔을 두는 이유는
계획서가 이 자리를 "아무도 결정하지 않은 채 바뀐다"고 부른 것이기 때문이다 — 결정했다는 사실이
코드에 남아야 한다.

`--human`이 `dropped_turns`를 말하지 않는 것도 같은 자리의 결정이다: 요청마다 한 줄씩 경고를
찍으면 스트리밍되는 답 사이에 노이즈가 된다. `--tui`가 패널에서 말하고 `--jsonl`이 필드로
나른다. §7-4에 남긴다.

### 4.6 telemetry-log의 설정 표면

```toml
[plugins."rivet.telemetry-log"]
# 구독 접두사. 생략하면 기본값이고, 기본값은 대화 **양쪽**을 뺀다 --
# `agent.text.`(모델이 말한 것)와 `agent.input.`(사용자가 친 것) 둘 다.
# topics = ["agent.run.", "agent.turn.", "agent.request.", "tool.", "plugin.", "runtime."]
level = "info"

# 대화 전문을 로그에 넣는다. 이제 **양쪽**이다: 한 플래그가 두 접두사를 민다.
# false(기본)면 둘 다 구독하지도 않는다 -- 길이만 세는 것도 안 한다.
# 좁은 프로파일은 둘 다 주지 않으므로, 이 플래그가 켜진 채로는 로드에 실패한다.
include_conversation = false
```

`broken_promises`가 좁은 프로파일 아래에서 이제 **둘**을 돌려준다는 것이 운영자에게 보이는
가장 큰 차이이고, `config.md:204-205`의 표 두 행이 그래서 움직인다(4b.8 행 7).

---

## 5. 실패 모드

| # | 실패 | 처리 |
|---|---|---|
| 1 | turn 1을 Ctrl-C로 취소하고 turn 2를 친다 | `CurrentTurn::begin`이 새 토큰을 만든다. 래치는 turn 1의 토큰에만 남는다 (DoD 3의 `a_cancelled_turn_does_not_cancel_the_next_one`) |
| 2 | turn 2를 Ctrl-C로 취소한다 | 펌프가 `CurrentTurn`에서 **지금** 토큰을 꺼내 취소한다. turn 1의 죽은 토큰이 아니다 (`a_later_turn_can_still_be_cancelled`) |
| 3 | turn 1에서 Ctrl-C를 두 번 눌러 놓고 turn 2에서 한 번 누른다 | `begin`이 `asked`도 함께 지웠으므로 `CancelTheRun`이다. 프로세스가 죽지 않는다 (`the_two_step_cancel_resets_each_turn`) |
| 4 | turn 1이 답 없는 tool call을 남기고 끝났다 | turn 2가 `close_interrupted`를 먼저 지난다. 안 지나면 `inspect`가 다음 turn에 `Rli::Broken`을 돌려주고 `rivet resume`이 **영구히** 거절한다([`session_recovery.rs:177-183`](../../crates/rivet-runtime/src/session_recovery.rs)) |
| 5 | 대화가 1,000 이벤트를 넘긴다 | `close_interrupted` 안의 `read_all`이 `READ_PAGE = 10_000`으로 끝까지 페이징한다. `store.read(id, 1, 1_000)`은 삭제된다 |
| 6 | 로그가 append로 고칠 수 없게 됐다(`Rli::Broken`) | `converse`의 `?`가 `Err(Storage)`로 나가고 대화가 끝난다. teardown은 `drive`가 그 뒤에서 돌린다. 문구는 이미 `rivet session fork`를 가리킨다 |
| 7 | 채널이 꽉 찬 상태에서 프롬프트를 제출한다 | 제어 채널이 차 있어도 프롬프트 채널은 별개다. 프롬프트 채널이 차 있으면(직전 것을 호스트가 아직 안 집었다) 초안이 컴포저에 **남고** `unsent`가 켜진다 — 어느 쪽으로도 사라지지 않는다 |
| 8 | 컴포저에 타이핑하는 중 승인 모달이 뜬다 | `request`가 컴포저를 재우고 초안을 보존한다. 모달이 답을 받으면 되돌아온다. 승인이 매달리는 경로가 생기지 않는다 |
| 9 | 승인 모달이 떠 있는 동안 Ctrl-C | 오늘과 같다: 모달은 그 키를 안 가져가고, 취소가 dispatcher를 깨워 모달을 치운다([`app.rs:196-206`](../../crates/rivet-tui/src/app.rs)) |
| 10 | run 도중 `q` | `StopDrawing` → `dismiss()` → 프롬프트 송신단이 떨어진다. run은 shutdown까지 계속되고(Phase 4의 약속), 끝나면 `recv()`가 `None`이라 대화가 끝난다 |
| 11 | `q` 뒤에 승인이 필요해진다 | dismiss된 `Tui`가 `Err`를 돌려주고 `Approvals`가 `Denied`로 접는다([`approval.rs:236`](../../crates/rivet-runtime/src/approval.rs)). Phase 4가 닫은 경로 그대로 |
| 12 | 버스가 `agent.input.received`를 떨어뜨린다 | 그 turn의 화면에는 구멍이 남고 상태바가 `≥N dropped`로 그것을 말한다([`draw.rs:250-254`](../../crates/rivet-tui/src/draw.rs)). **다음 turn 경계에서** `sync_transcript`가 로그에서 통째로 다시 만들어 메운다 |
| 13 | 대화가 64 KB를 넘는다 | 앞에서 문자를 덜어내고 문자 수를 보고한다. 패널이 `"… N character(s) elided"`로 말하고, 전문은 `rivet session show`에 있다 |
| 14 | 모델이 앞선 turn을 컨텍스트에서 잃는다 | `history::trim`이 세고([`history.rs:102-110`](../../crates/rivet-runtime/src/context/history.rs)) `dropped_turns`가 나르고 패널이 `"… N earlier turn(s) are no longer sent to the model"`로 말한다. **무엇을 버릴지 고르는 전략은 이 Phase가 건드리지 않는다** (§11-1) |
| 15 | turn 1이 한도로 끝나고 turn 5가 정상으로 끝난다 | 종료 코드는 0이다. 마지막 turn이 정한다 (§2.1-h) |
| 16 | `rivet resume --tui`로 이미 끝난 세션을 연다 | `AskForMore` → 화면이 컴포저로 열린다. 아무것도 안 묻고 `q`면 `NothingAsked` → exit 0 |
| 17 | `rivet resume`(비-tui)으로 이미 끝난 세션을 연다 | `AskForMore`의 문구를 찍고 exit 2. **오늘과 같다** — `resuming_a_finished_session_is_refused`([`e2e.rs:621`](../../crates/rivet-cli/tests/e2e.rs))가 그대로 통과한다 |
| 18 | `rivet resume --tui`로 `closed` 세션을 연다 | `Refuse` 그대로. raw mode에 **들어가기 전에** 거절한다 — `resume`은 `observe(output)?`([`run.rs:202`](../../crates/rivet-cli/src/run.rs)) 앞에서 판정한다 |
| 19 | teardown 중(`unload_all`) 매달린다 | 펌프가 살아 있으므로 두 번째 Ctrl-C가 `ForceExit`로 터미널을 되돌리고 나간다([`run.rs:558-567`](../../crates/rivet-cli/src/run.rs)). 비-tui에서는 신호 처리기가 이미 abort된 뒤인데, **그것은 오늘도 그렇다** |

### 알려진 미해결 — 그대로 물려받는 것 셋

**`Plugin::load`/`unload`의 타임아웃은 여전히 없다**([`architecture.md` §11-15](../architecture.md)).
이 Phase는 그 위에 하나를 더 얹는다: 이제 그 매달림이 **대화의 끝**에 있으므로 사용자가 답을
다 받은 뒤에 나가지 못하는 모양으로 보인다. 펌프가 출구를 주지만 데드라인이 생긴 것은 아니다.

**컨텍스트 압축(§11-1)은 닫지 않는다.** `SessionEvent::Checkpoint`에는 아직 기록자가 없고
(비-테스트 참조가 [`session.rs:377-395`](../../crates/rivet-core/src/session.rs)의 `apply` 팔
하나뿐이다), 이 Phase는 `dropped_turns`를 **화면에 올릴** 뿐이다. 다만 이 Phase 이후로 그
질문은 "언젠가 정할 것"이 아니라 사용자가 매일 보는 줄이 된다.

**`max_duration_ms`가 turn 단위가 된다.** `Watchdog`은 `run()`마다 시작하므로
([`agent_loop.rs:215`](../../crates/rivet-runtime/src/agent_loop.rs)) 대화 전체가 아니라 turn마다
예산을 받는다. 그 편이 옳아 보인다 — 사람이 사이에 앉아 있는 대화에 벽시계 총량 예산을 거는
것은 뜻이 다르다 — 그러나 Phase 4의 PR 리뷰 blocking이 이 숫자 위에서 벌어졌으므로 적어 둔다.
`max_turns`·`max_total_tokens`도 같다: **run 단위이고, 대화 단위 예산은 이 Phase가 만들지
않는다.** §7-5.

---

## 6. 테스트 전략

전부 offline (`cargo test --workspace --offline`).

**기준선은 `a41d115`에서 실측한 수다.** 계획서의 진행 추적 표가 적은 Phase 4의 772는 Phase 4
**종료 시점**의 수이고, 이 브랜치의 base에는 그 뒤의 커밋이 얹혀 있다 — 두 수가 갈라질 수
있다는 것을 계획서 표의 각주가 이미 적어 두었다. 그러므로 기준선은 **구현 시작 시점에 base에서
다시 돌려** 정하고, 그 수를 유지하거나 올린다.

**설계 시점의 참고값**(이 문서를 쓴 날, base `a41d115`에서 `cargo test --workspace --offline`):
**772 passed · 0 failed · 1 ignored · 63 suites.** 유일한 ignored는
`plugins/model-openai`의 `live_round_trip`(실제 provider가 필요해 `RIVET_LIVE=1`로만 돈다)이다.
이번에는 계획서 표의 772와 **같다** — Phase 4 종료 뒤 이 브랜치까지 테스트를 더한 커밋이
없었다는 뜻이다. 이 수를 적어 두는 이유는 목표로 삼기 위해서가 아니라, 구현이 기준선을
**낮추는** 방향으로 어긋났을 때 알아챌 좌표가 문서에 하나는 있어야 하기 때문이다.

**사라지는 테스트는 하나도 없다.** §6.0이 손대는 것은 전부 필드 이름이나 기대값이 바뀌는
것이지 없어지는 것이 아니다. 삭제되는 **코드**는 있다(`answer`), 그리고 그 코드에는 테스트가
없었다.

### 6.0 기존 테스트에 무엇이 일어나는가

#### 어떻게 훑었나 — 재현할 수 있는 세 갈래

전부 base `a41d115`에서 돌렸다.

**① 바뀌는 심볼로.** 이 설계가 이름을 바꾸거나 없애거나 형을 바꾸는 것을 워크스페이스 전체에서
찾았다: `RunView` / `run.text` / `text_dropped` · `TEXT_LIMIT` · `Tui::new` · `Intent` ·
`Screen::start` · `answer(` · `report(` · `drive(` · `CONVERSATION_TOPIC` · `ResumePlan` /
`resume_plan` · `AgentEvent::RequestStarted` · `AssembledContext` / `.dropped` /
`dropped_report` · `run::resume`.

**② 단언되는 리터럴로.** 심볼을 하나도 지나가지 않는 단언이 있다. `"elided"` ·
`"[q] quit"` · `"already finished"` · `"agent.text."` · `"history.turns:"` ·
`"scrolled out of the panel"` · `"Phase 5"`.

**③ 저장소의 실제 파일을 읽는 테스트로.** `rivet.example.toml`을 읽는 셋
(`the_shipped_example_loads` [`config.rs:800`](../../crates/rivet-cli/src/config.rs) ·
`the_shipped_example_keeps_its_named_agent` [`config.rs:854`](../../crates/rivet-cli/src/config.rs) ·
`every_id_the_shipped_example_enables_resolves` [`catalog.rs:365`](../../crates/rivet-cli/src/catalog.rs))과
`Cargo.toml`을 파싱하는 하나(`the_tui_crate_does_not_depend_on_the_runtime`
[`independence.rs:17`](../../crates/rivet-tui/tests/independence.rs)). **넷 다 그대로 통과한다** —
4b.8이 예제 파일에서 고치는 것은 주석뿐이고, `rivet-tui`의 의존 목록은 안 바뀐다.

#### 표 — 이 Phase가 건드리는 기존 단언 전부

| 테스트 | 어디 | 무엇이 바뀌나 | 그 뒤에 무엇이 그 약속을 붙드나 |
|---|---|---|---|
| `streamed_text_is_capped_and_says_what_it_dropped` | [`state.rs:442`](../../crates/rivet-tui/src/state.rs) | 필드 이름, **그리고 입력 크기**. 오늘의 200 × 100 바이트 = 20 KB는 새 상한(64 KB) **아래**라 아무것도 안 잘린다 — 반복을 1,000회로 올린다. 상한을 올리면서 이 조정을 빠뜨리면 테스트가 조용히 아무것도 안 지키게 된다 | 자기 자신. 성질(유계 + 침묵하지 않는다)은 그대로다 |
| `what_it_says_it_dropped_is_characters_not_bytes` | [`state.rs:454`](../../crates/rivet-tui/src/state.rs) | 같은 두 가지. 오늘의 10 × 1,000자 × 3바이트 = 30 KB도 새 상한 아래다 — 반복을 30회로 올리고 합계 단언(`+ kept == 10_000`)을 30,000으로 맞춘다. **이 테스트는 0이 잘려도 통과하므로** 크기 조정이 특히 중요하다 | 자기 자신. **이 성질을 잃지 않으려고 항목이 아니라 문자를 버리기로 했다** (§2.2) |
| `every_panel_is_filled_from_events_alone` | [`independence.rs:48`](../../crates/rivet-tui/tests/independence.rs) | `state.run.text.contains(...)`([`:122`](../../crates/rivet-tui/tests/independence.rs)) → transcript. 그리고 `AgentEvent::InputReceived` 봉투가 목록에 **추가**된다 | 자기 자신 + 새 테스트 `the_transcript_shows_the_user_turn_from_events_alone` |
| `the_agent_panel_follows_a_run_from_start_to_stop` | [`panels.rs:46`](../../crates/rivet-tui/tests/panels.rs) | 없음. `"looking"`은 transcript에서도 그려진다 | — |
| `an_elided_ring_buffer_says_how_much_it_dropped` | [`panels.rs:256`](../../crates/rivet-tui/tests/panels.rs) | 입력 크기만. 문장(`"elided"`)도 출처를 바꾼 패널도 같은 줄을 그리지만, 300 × 100 = 30 KB는 새 상한 아래다 — 반복을 1,000회로 | — |
| `the_three_panels_fit_an_eighty_column_terminal` | [`panels.rs:226`](../../crates/rivet-tui/tests/panels.rs) | 없음 — **그러나 공짜가 아니다.** 상태바는 여전히 `rows[23]`이고 컴포저가 `rows[22]`를 가져가므로 24행·80칼럼 단언([`:244-247`](../../crates/rivet-tui/tests/panels.rs))은 그대로다. `rows[23].contains("[q] quit")`([`:252`](../../crates/rivet-tui/tests/panels.rs))가 그대로인 것은 §4.4가 `Watching` 힌트를 33자로 **고정했기** 때문이다. 힌트를 늘리면 이 단언이 깨진다 | 자기 자신 + 새 테스트 `the_composer_sits_between_the_panels_and_the_status_bar` · `a_composing_status_bar_still_says_how_to_get_out` |
| `quitting_asks_the_host_rather_than_reaching_for_the_token` | [`app.rs:374`](../../crates/rivet-tui/src/app.rs) | `intents.try_recv()` → `intents.control.try_recv()` | 자기 자신 |
| `ctrl_c_in_the_tui_asks_for_a_cancel` | [`app.rs:391`](../../crates/rivet-tui/src/app.rs) | 같은 한 줄. **의미는 안 바뀐다** — 기본 모드가 `Watching`이고 수식자가 CONTROL이다. DoD 4가 "그대로 통과해야 한다"고 지목한 것 | 자기 자신 + `ctrl_c_still_cancels_while_composing` |
| `a_key_release_is_not_a_second_press` | [`app.rs:403`](../../crates/rivet-tui/src/app.rs) | 같은 한 줄 | 자기 자신 |
| `tab_moves_focus_without_telling_the_host_anything` | [`app.rs:413`](../../crates/rivet-tui/src/app.rs) | 같은 한 줄 | 자기 자신 + `tab_is_inert_while_composing` |
| `cancelling_is_never_swallowed_by_the_modal` | [`app.rs:577`](../../crates/rivet-tui/src/app.rs) | 같은 한 줄. 맨 `c`를 쓰는데 모드가 `Watching`이라 결과가 같다 | 자기 자신 |
| `a_second_cancel_intent_forces_the_exit` | [`run.rs:602`](../../crates/rivet-cli/src/run.rs) | 없음 — `Reaction::to`는 안 바뀐다 | 계획서가 지적한 대로 **이 테스트는 회귀를 못 본다.** 그것을 보는 것이 새 `the_two_step_cancel_resets_each_turn` |
| `quitting_leaves_the_ui_without_stopping_the_run` | [`run.rs:631`](../../crates/rivet-cli/src/run.rs) | 없음 | — |
| `quitting_also_stops_the_screen_from_answering_approvals` | [`run.rs:643`](../../crates/rivet-cli/src/run.rs) | `Tui::new()`의 반환 모양만 | 자기 자신 + 새 `quitting_ends_the_conversation_rather_than_only_the_drawing` |
| `a_dismissed_screen_is_not_a_sink` | [`app.rs:600`](../../crates/rivet-tui/src/app.rs) | `Tui::new()`의 반환 모양만. Phase 4의 이 단언은 그대로 성립한다(DoD 6이 그렇게 적었다) | — |
| `every_bus_topic_is_claimed` | [`event_flow.rs:73`](../../crates/rivet-runtime/tests/event_flow.rs) | `owner`의 agent 팔([`:77-84`](../../crates/rivet-runtime/tests/event_flow.rs))에 `InputReceived`, `published` 목록([`:133-157`](../../crates/rivet-runtime/tests/event_flow.rs))에 `"agent.input.received"`가 `"agent.run.started"` 다음에 | 자기 자신. **이 테스트가 깨지는 것이 설계가 원하는 바다** |
| `every_topic_is_granted_or_deliberately_withheld` | [`config.rs:680`](../../crates/rivet-cli/src/config.rs) | `should_reach_a_narrowed_profile`([`:688-698`](../../crates/rivet-cli/src/config.rs))에 `InputReceived => false` | 자기 자신 + 새 `the_prompt_echo_is_withheld_from_a_narrowed_profile` |
| `model_events_carry_correlation` | [`bus.rs:542`](../../crates/rivet-runtime/src/bus.rs) | `RequestStarted` 리터럴에 `dropped_turns: 0` | — |
| `dropped_history_is_reported` | [`context/mod.rs:381`](../../crates/rivet-runtime/src/context/mod.rs) | `d.key.starts_with("history.turns:")` → `assembled.dropped_turns == 1`. **이 테스트가 이 설계의 근거다** — 단언이 문자열 키를 파싱하고 있었다 | 자기 자신, 이제 필드로 |
| `the_default_topics_leave_out_the_conversation` | [`lib.rs:364`](../../plugins/telemetry-log/src/lib.rs) | `CONVERSATION_TOPIC` → 둘 다 순회 | 자기 자신 |
| `include_conversation_off_means_it_does_not_even_subscribe` | [`lib.rs:378`](../../plugins/telemetry-log/src/lib.rs) | 같은 순회 | 자기 자신 |
| `include_conversation_is_a_promise_even_without_a_topics_list` | [`lib.rs:425`](../../plugins/telemetry-log/src/lib.rs) | `broken_promises`가 **둘**을 돌려준다 | 자기 자신 |
| `a_written_prefix_that_reaches_the_conversation_needs_include_conversation` | [`lib.rs:491`](../../plugins/telemetry-log/src/lib.rs) | **없음.** 세 모양([`:496`](../../plugins/telemetry-log/src/lib.rs))이 전부 `agent.text.` 쪽이라 고치기 전에도 통과한다 — DoD 8이 그래서 새 테스트를 요구한다 | 새 `an_input_prefix_also_needs_include_conversation` |
| `saying_you_meant_it_is_accepted_and_is_a_promise` | [`lib.rs:510`](../../plugins/telemetry-log/src/lib.rs) | **없음.** `agent.`가 두 접두사를 다 덮으므로 둘 다 안 밀린다 | — |
| `a_prefix_narrower_than_the_conversation_is_not_the_conversation` | [`lib.rs:525`](../../plugins/telemetry-log/src/lib.rs) | **없음.** `agent.turn.`은 어느 쪽과도 겹치지 않는다 | — |
| `a_readonly_profile_refuses_a_telemetry_plugin_that_was_told_to_log_the_conversation` | [`telemetry.rs:115`](../../plugins/telemetry-log/tests/telemetry.rs) | `error.to_string().contains(CONVERSATION_TOPIC)`([`:126`](../../plugins/telemetry-log/tests/telemetry.rs)) → 배열의 첫 원소, 또는 둘 다 | 자기 자신 |
| `resume_refuses_a_finished_conversation` | [`session_recovery.rs:462`](../../crates/rivet-runtime/src/session_recovery.rs) | `Refuse` → `AskForMore`. **이름도 바뀐다**: `resume_asks_for_more_after_a_finished_conversation` | 자기 자신. 거절이 아니라 "새 프롬프트가 있으면 이어 갈 수 있다"가 된 것이 이 Phase의 결정이다 (§2.1-h) |
| `a_finished_session_is_not_resumed` | [`tests/session_recovery.rs:504`](../../crates/rivet-runtime/tests/session_recovery.rs) | 같은 변이 갈아타기 + 이름을 `a_finished_session_is_asked_for_more` | 자기 자신 |
| `resume_refuses_a_closed_session` · `resume_refuses_an_empty_session` | [`session_recovery.rs:474`](../../crates/rivet-runtime/src/session_recovery.rs) · [`:482`](../../crates/rivet-runtime/src/session_recovery.rs) | **없음.** 둘 다 `Refuse` 그대로다 | — |
| `resuming_a_finished_session_is_refused` | [`e2e.rs:621`](../../crates/rivet-cli/tests/e2e.rs) | **없음.** 비-tui에서는 문구도 종료 코드(2)도 그대로다 | — |
| `tui_refuses_a_pipe` | [`e2e.rs:588`](../../crates/rivet-cli/tests/e2e.rs) | **없음** | — |
| `the_last_frame_shows_the_state_the_run_ended_in` | [`app.rs:428`](../../crates/rivet-tui/src/app.rs) | `Tui::new()`의 반환 모양만 | — |
| `the_keys_map_to_the_three_outcomes` · `the_remember_key_is_inert_when_the_policy_did_not_offer_it` · `a_dropped_receiver_clears_the_pending_modal` · `dismissing_releases_an_approval_that_was_already_waiting` · `a_render_loop_that_ends_on_its_own_stops_being_a_sink` · `events_fold_into_the_screen` | [`app.rs:500`](../../crates/rivet-tui/src/app.rs) 이하 | `Tui::new()`의 반환 모양만 | — |
| `an_approval_modal_is_drawn_from_state_alone` · `a_policy_that_will_not_be_remembered_does_not_offer_the_key` | [`approval.rs:26`](../../crates/rivet-tui/tests/approval.rs) · [`:51`](../../crates/rivet-tui/tests/approval.rs) | **없음.** 손으로 만든 `AppState`가 입력이고 새 필드는 전부 `Default`가 있다 | — |
| `a_narrow_status_bar_keeps_the_segments_a_user_cannot_work_without` | [`draw.rs:304`](../../crates/rivet-tui/src/draw.rs) | 없음 — **그리고 이 테스트가 §4.4의 33자 제약이 존재하는 이유다.** 80칼럼에서 힌트 차례의 남은 예산이 34자이므로, 힌트가 그보다 길면 `fit`이 통째로 버리고 `contains("[q] quit")`([`:312`](../../crates/rivet-tui/src/draw.rs))와 `!contains(" pl")`([`:314`](../../crates/rivet-tui/src/draw.rs))이 **둘 다** 깨진다. `Watching` 힌트를 한 글자도 안 늘리기로 한 것이 그 답이다 | 자기 자신 + 새 테스트 `neither_mode_hint_outgrows_the_status_bar` |

### 6.1 DoD 10줄 — 각각 무엇이 증명하는가

| DoD | 테스트 | 어디 | 무엇을 붙드나 |
|---|---|---|---|
| 1 화면이 run보다 오래 산다 | `the_host_does_not_stop_the_screen_when_a_run_ends` | `run.rs` 단위 | 가짜 `Turns` + 프롬프트 하나가 든 채널로 `converse`를 돌리고, **가짜가 두 번 불렸는지**와 첫 turn 뒤 `!tui.is_dismissed()`를 단언한다. 계획서가 요구한 "호스트 수준"이 이것이다 — fold 수준 단언은 오늘도 참이라 아무것도 못 붙든다. 터미널이 필요 없다 |
| 1 (짝) | `a_second_prompt_continues_the_same_session` | `run.rs` 단위 | 진짜 `JsonlSessionStore`를 임시 디렉터리에 두고, 가짜 `Turns`가 turn 사이에 assistant 메시지를 append한다. 두 turn의 `SessionId`가 같고 turn 2의 `state.messages`가 turn 1의 사용자 메시지를 담는다 |
| 2 수리하고 나서, 끝까지 읽는다 | `each_turn_repairs_the_log_before_it_reads_it` | `run.rs` 단위 | 가짜 `Turns`가 turn 1에서 답 없는 tool call을 로그에 남긴다. turn 2가 받은 `state.messages`에 `inspect`가 `Rli::Broken`이 **아니**라고 답한다 |
| 2 (짝) | `a_conversation_past_one_read_page_keeps_its_middle` | `run.rs` 단위 | 1,000을 넘는 이벤트를 심고 한 turn을 돌린 뒤 `state.last_seq`가 로그의 끝과 같다. `store.read(id, 1, 1_000)`이 되살아나면 깨진다 |
| 3 취소가 turn 경계를 안 넘는다 | `a_cancelled_turn_does_not_cancel_the_next_one` | `run.rs` 단위 | `begin()` → 취소 → `begin()` → 새 토큰이 취소돼 있지 않다 |
| 3 (짝) | `a_later_turn_can_still_be_cancelled` | `run.rs` 단위 | `begin()` 두 번 뒤 `react(Cancel)`이 **둘째** 토큰을 취소한다. 계획서가 지목한 셋째 테스트이고, 없으면 "펌프가 죽은 토큰을 취소한다"가 앞의 둘을 다 통과한다 |
| 3 (짝) | `the_two_step_cancel_resets_each_turn` | `run.rs` 단위 | `react`가 `CancelTheRun` → `ForceExit` → `begin()` → 다시 `CancelTheRun`. **`react`가 펌프의 몸통 전부**이므로 이것이 계획서가 요구한 "펌프 수준"이다. `carry_out`은 부르지 않는다 — `ForceExit`는 `process::exit`다 |
| 4 컴포저가 키를 안 삼킨다 | `ctrl_c_still_cancels_while_composing` | `app.rs` 단위 | 모드를 `Composing`으로 두고 Ctrl-C → `Intent::Cancel` |
| 4 (짝) | `a_typed_key_is_text_rather_than_a_command_while_composing` | `app.rs` 단위 | 맨 `c`와 `q`가 초안에 붙고 채널에는 아무것도 안 간다 |
| 4 (짝) | `an_approval_modal_does_not_eat_the_composer` | `app.rs` 단위 | 초안을 치고 `request`를 띄우고 `n`으로 답한다. 초안이 그대로이고 모드가 `Composing`으로 돌아온다 |
| 5 프롬프트는 안 버려진다 | `a_submitted_prompt_is_never_dropped` | `app.rs` 단위 | 제어 채널을 여덟 번 채워 놓고 제출한다. 호스트가 받는다. 그리고 프롬프트 채널이 찬 경우에는 초안이 화면에 남는다는 짝 단언 |
| 6 `q`가 대화를 끝낸다 | `quitting_ends_the_conversation_rather_than_only_the_drawing` | `run.rs` 단위 | turn 1 뒤 `dismiss()`를 부르면 `converse`가 정확히 한 turn만 돌고 나온다. `a_dismissed_screen_is_not_a_sink`는 그대로 성립한다 |
| 7 사용자 turn이 이벤트만으로 | `the_transcript_shows_the_user_turn_from_events_alone` | `independence.rs` | 손으로 만든 `AgentEvent::InputReceived` 봉투 하나만으로 transcript에 `Entry::User`가 생긴다. `every_panel_is_filled_from_events_alone`의 확장 |
| 7 (짝) | `a_new_turn_clears_what_belonged_to_the_last_one` | `state.rs` 단위 | `RequestFailed`로 `retries`·`last_error`를 세운 뒤 `RunStarted`를 접으면 둘 다 지워지고 transcript는 남는다 |
| 8 좁은 프로파일 · 양쪽 덮기 | `the_prompt_echo_is_withheld_from_a_narrowed_profile` | `subscriber.rs` | Phase 3의 `a_readonly_profile_keeps_the_conversation_from_a_subscriber`([`:353`](../../crates/rivet-plugin/tests/subscriber.rs))의 짝. 모든 토픽을 원하는 구독자가 `agent.input.received`를 **한 번도** 못 받는다 |
| 8 (짝) | `an_input_prefix_also_needs_include_conversation` | `telemetry-log` 단위 | `topics = ["agent.input."]`·`["agent.input.received"]`가 플래그 없이 거절되고, `include_conversation = true`가 접두사 **둘 다**를 밀어 넣는다 |
| 9 산문 열다섯 곳 | — | — | 체크박스. 계획서의 `### 4b.8` 표 + §8-6의 세 행. **컴파일러도 테스트도 한 곳을 막아 주지 않는다** |
| 10 재출력 삭제 · 요약 위치 | `a_conversation_does_not_reprint_its_own_transcript` | — | **자동화하지 않는다.** §6.4 |
| 10 (짝) | `the_summary_is_printed_after_the_terminal_comes_back` | — | **타입으로 답한다.** `report`가 `&Restored`를 요구하고 `Restored`를 만드는 것은 `Screen::stop`(await한다)과 "화면이 없었다"뿐이다. §6.4 |

### 6.2 작업 항목별 새 테스트

**4b.1 — `Prompt`과 두 채널** (`crates/rivet-tui/src/app.rs`)
- `a_submitted_prompt_is_never_dropped` (DoD 5)
- `a_prompt_that_cannot_be_handed_over_stays_in_the_composer` — 용량 1이 찬 상태에서 제출하면
  초안이 남고 `unsent`가 켜진다
- `a_dismissed_screen_stops_being_a_place_prompts_come_from` — `dismiss()` 뒤 `recv()`가 `None`

**4b.2 — 모드와 키 맵** (`crates/rivet-tui/src/app.rs`, `draw.rs`)
- `ctrl_c_still_cancels_while_composing` · `a_typed_key_is_text_rather_than_a_command_while_composing` ·
  `an_approval_modal_does_not_eat_the_composer` (DoD 4)
- `esc_leaves_the_composer_rather_than_the_conversation` — `Composing`의 `Esc`가 `Quit`이
  아니고 초안이 남는다
- `a_modal_does_not_let_enter_open_the_composer_underneath_it` — 모달이 떠 있는 동안 `Enter`가
  모드를 바꾸지 않는다. `answer_approval`이 `Enter`를 `_ => return false`로 흘려보내므로
  ([`app.rs:217`](../../crates/rivet-tui/src/app.rs)) 4단계까지 닿는 유일한 모드 전환 키이고,
  `an_approval_modal_does_not_eat_the_composer`는 이 경로를 겨냥하지 않는다
- `tab_is_inert_while_composing`
- `an_empty_draft_is_not_a_turn` — 빈 초안에 `Enter`는 채널로 아무것도 안 보낸다
- `the_composer_sits_between_the_panels_and_the_status_bar` — 80×24에서 컴포저가 `rows[22]`,
  상태바가 `rows[23]`, 24행 전부 80칼럼
- `a_watching_composer_line_says_how_to_start_typing` — `Watching`의 컴포저 줄이
  `press [enter] to type`를 담고, `Composing`에서는 초안과 `> `가 그 자리를 대신한다
- **`neither_mode_hint_outgrows_the_status_bar`** — §4.4의 33자 제약을 **규칙으로** 붙든다:
  두 모드의 힌트 문자열 길이가 오늘의 것(33) 이하임을 단언한다. 이것이 없으면 다음 사람이
  힌트에 한 단어를 더했다가 80칼럼 단언 셋이 왜 깨졌는지 역추적해야 한다
- **`a_composing_status_bar_still_says_how_to_get_out`** — 규칙의 결과를 붙든다: 폭 80,
  모델이 설정된 상태, 모드 `Composing`에서 상태바가 `[esc] leave`를 담는다. `Watching`의
  같은 단언은 `a_narrow_status_bar_keeps_the_segments_a_user_cannot_work_without`가 이미 한다

**4b.3 — 토픽** (`rivet-core`, `rivet-runtime`)
- `a_run_publishes_the_prompt_it_was_given` — `agent_loop` 테스트. `input`이 `Some`이면
  `agent.input.received`가 정확히 한 번, 그리고 `agent.run.started` **뒤에** 나간다
- `a_resumed_run_publishes_no_prompt` — `input`이 `None`이면 안 나간다
- `every_bus_topic_is_claimed`([`event_flow.rs:73`](../../crates/rivet-runtime/tests/event_flow.rs))의
  목록 갱신 (§6.0)

**4b.4 — 소비자** (`rivet-cli`, `rivet-plugin`, `telemetry-log`)
- `the_prompt_echo_is_withheld_from_a_narrowed_profile` (DoD 8)
- `an_input_prefix_also_needs_include_conversation` (DoD 8)
- `include_conversation_covers_both_halves` — 플래그만 켜면 `topics`에 접두사 **둘**이 들어간다
- `the_human_renderer_does_not_echo_the_prompt` — `HumanRenderer`가 `InputReceived`에 아무것도
  안 찍는다. `_ => {}`로 조용히 지나가던 것을 결정으로 만든 자리를 붙든다

**4b.5 — transcript** (`rivet-tui`)
- `the_transcript_shows_the_user_turn_from_events_alone` · `a_new_turn_clears_what_belonged_to_the_last_one` (DoD 7)
- `a_rebuilt_transcript_matches_the_log` — 손으로 만든 `Message` 배열에서
  `rebuild_transcript`가 만든 것이 fold가 만들었을 것과 같다
- `a_rebuild_restores_a_user_turn_the_bus_dropped` — 봉투를 일부러 빼먹고 fold한 뒤 rebuild가
  메운다
- `a_rebuild_is_idempotent`
- `the_panel_says_how_many_turns_the_model_has_stopped_seeing` — `dropped_turns`가 그려진다

**4b.6 — turn 루프** (`rivet-cli/src/run.rs`)
- DoD 1·2·3·6의 여덟 (§6.1)
- `a_conversation_reports_the_last_turns_summary` — `converse`가 마지막 요약을 돌려준다
- `a_resume_that_asks_for_more_opens_without_running_a_turn` — `FirstTurn::Ask`가 첫 turn을
  돌리지 않고 프롬프트를 기다린다
- `a_non_interactive_run_still_runs_exactly_one_turn` — `prompts`가 `None`이면 한 turn

**4b.7 — 삭제와 위치**
- 자동화하지 않는 둘 (§6.4). `answer`의 삭제는 diff에 보이고, `report`의 위치는 `Restored`가
  타입으로 강제한다

### 6.3 게이트

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

`missing_debug_implementations`가 `warn` + `-D warnings`이므로 새 공개 타입은 전부 `Debug`를
가진다: `Prompt` · `Intents` · `InputMode` · `Composer` · `Transcript` · `Entry` ·
`Resumed`. 비공개 타입 `CurrentTurn` · `TurnState` · `FirstTurn` · `Restored` · `Assembled`도
`Debug`를 붙인다 — 그 lint는 공개 타입만 보지만, 이 저장소의 나머지가 그렇다.

타이밍에 의존하는 새 테스트는 없다. `converse` 테스트는 전부 가짜 `Turns`와 채널로 돌고,
`sleep`이 필요한 것은 기존 승인 테스트뿐이다.

### 6.4 자동화하지 않는 것, 그리고 그 대신 무엇이 서 있나

**`--tui`의 실제 raw mode 경로는 여전히 자동화 테스트가 없다.** Phase 3이 남긴 것이고
(`docs/plan.md` §"미검증으로 남긴 것") 이 Phase도 닫지 않는다. e2e 하네스는 stdout을 파이프로
받으므로([`support/mod.rs:305-306`](../../crates/rivet-cli/tests/support/mod.rs)) `--tui`가
애초에 거절된다([`run.rs:138-143`](../../crates/rivet-cli/src/run.rs),
`tui_refuses_a_pipe` [`e2e.rs:588`](../../crates/rivet-cli/tests/e2e.rs)). pty 하네스를 들이는
것은 이 Phase의 항목이 아니다.

그래서 DoD 10의 두 줄은 이렇게 답한다:

- **`a_conversation_does_not_reprint_its_own_transcript`** — 쓰지 않는다. 붙들 것은 함수의
  **부재**이고, `answer`([`run.rs:380-400`](../../crates/rivet-cli/src/run.rs))가 사라지는 것은
  diff에 보인다. `--human`·`--jsonl`은 애초에 그 경로를 지나지 않았으므로 e2e로 바꿔 쓸 수도
  없다.
- **`the_summary_is_printed_after_the_terminal_comes_back`** — 테스트가 아니라 **타입**이
  답한다. `report(&Restored, &RunSummary)`이고 `Restored`를 만드는 문은 둘뿐이다:
  `Screen::stop`(렌더 태스크를 **await**한다) 과 `Restored::nothing_to_restore()`(화면이 없었다).
  화면이 살아 있는 동안 요약을 찍는 코드는 **쓸 수가 없다**. 이 저장소에는 선례가 있다 —
  `the_tui_crate_does_not_depend_on_the_runtime`이 지키는 것도 테스트가 아니라 컴파일 성질이고
  ([`independence.rs:1-6`](../../crates/rivet-tui/tests/independence.rs)), Phase 4의 argv 수정도
  "규칙을 타입으로 옮겼다"였다.

그리고 계획서의 검증 명령이 손으로 볼 것을 이미 적어 두었다(`docs/plan.md`의 Phase 4b 블록):
답이 끝나도 화면이 남는지, 이어서 다음 질문이 되는지, `q`가 대화를 끝내는지, Ctrl-C가 여전히
취소인지.

---

## 7. 열린 질문

과제 서술이 정하지 않은 것들. 각각 이 설계가 쓰는 잠정 답을 **[가정]**으로 달아 두어 빌드가
막히지 않게 하되, 가정임을 숨기지 않는다.

1. **컴포저가 한 줄인가 여러 줄인가.** 여러 줄 프롬프트(붙여넣은 스택 트레이스)를 치고 싶은
   것은 분명하다. **[가정]** 한 줄. `Enter`가 보내기이므로 여러 줄에는 다른 제출 키
   (`Ctrl-D`?)가 필요하고, 그건 키 맵을 한 번 더 가른다. 줄바꿈이 든 프롬프트는 지금도
   `rivet "…"`로 보낼 수 있다.
2. **줄 편집이 어디까지인가.** 이 설계는 문자·Backspace·`Enter`·`Esc`뿐이다. 커서 이동, 단어
   삭제, 프롬프트 이력(↑)은 없다. **[가정]** 없이 간다. 있으면 좋지만 하나도 이 Phase의 DoD가
   아니고, 넣기 시작하면 `handle_key`가 줄 편집기가 된다.
3. **도구 줄을 transcript에 시간순으로 섞을 것인가.** 지금은 패널이 [transcript][빈 줄][도구
   줄들] 순이므로, 5 turn 대화는 전부의 텍스트 다음에 전부의 도구가 온다. **[가정]** 섞지
   않는다 (§2.2). 섞는 것은 `tool_index`가 항목 배열을 가리키게 만드는 일이고, 스크롤백이
   생기는 날 함께 하는 편이 낫다.
4. **`--human`이 `dropped_turns`를 말해야 하는가.** **[가정]** 안 말한다 (§4.5). 요청마다 한
   줄은 노이즈이고, "처음 0이 아니게 되는 순간에만"은 렌더러에 상태를 하나 더한다.
5. **대화 단위 예산이 필요한가.** `max_turns`·`max_total_tokens`·`max_duration_ms`가 전부 run
   단위가 된다. 스무 turn을 돌면 `max_turns = 50`을 스무 번 새로 받는다. **[가정]** 이 Phase는
   만들지 않는다. 사람이 사이에 앉아 있는 대화에 총량 예산을 거는 것은 무인 실행과 뜻이 다르고,
   Phase 5의 DoD(무개입 완주)가 그 축을 가져간다. §11 승격 후보다.
6. **버스 유실 보수를 매 turn 하는 것이 과한가.** `sync_transcript`는 turn마다 전체 함수를
   돌린다(메시지 수에 선형). **[가정]** 돌린다 — 그 자리에서 이미 로그 전체를 읽었고, 재계산이
   O(n)인데 그 옆의 모델 왕복은 초 단위다. 필요해지면 `state.last_seq`가 바뀐 경우에만 도는
   것으로 좁힐 수 있다.
7. **컴포저 초안이 `Esc` 뒤에도 남는 것이 옳은가.** 남긴다고 정했다(실수로 지우는 쪽이 더
   나쁘다). **[가정]** 그러나 `Watching`에서 초안이 남아 있다는 표시가 없으면 사용자가 잊는다 —
   컴포저 줄이 회색으로 초안을 계속 보여주는 것으로 갚는다.
8. **`AskForMore`가 `rivet-runtime`의 어휘여야 하는가, `rivet-cli`의 판단이어야 하는가.**
   `state.closed`를 CLI가 직접 보고 갈라도 된다. **[가정]** 런타임의 어휘로 둔다. `resume_plan`이
   이미 세 조건(닫힘 · 비어 있음 · 이미 답함)을 구분해 놓고 결과를 둘로 접고 있었으므로, 셋째
   변이는 있던 구분을 드러내는 것이지 새로 만드는 것이 아니다. 그리고 `rivet-runtime`을 직접
   쓰는 임베더도 같은 구분이 필요하다.

---

## 8. 계획과 다른 점 / 계획이 틀린 점

계획서는 확정이고 이 설계가 고치지 않는다. 설계하면서 발견한 것을 여기 적는다.

**1. `README.md:19`의 실제 문장은 "Phase 4b"가 아니라 "Phase 4.5"다.** 4b.8 표의 그 행은
`README.md:19` — "다음은 Phase 4b(대화형 TUI)"라고 인용하는데, 실제 줄은
```text
> 다음은 Phase 4.5(대화형 TUI). 설계 문서는 [`docs/`](./docs)에 있다.
```

줄 번호는 맞다.
개명(4.5 → 4b)이 `plan.md` 안에서만 일어났고 README가 따라오지 않았다. 그날 고칠 것이 하나가
아니라 **둘**이다: 이름과, 그것이 더 이상 "다음"이 아니라는 사실. 표의 "특히 잊기 쉽다"는
경고가 생각보다 더 맞다.

**2. 4b.7의 "사과를 그냥 지우면 아무 안내 없이 앞부분을 잃는다"는 절반만 맞다.** 패널 안의
안내는 `answer()`와 **별개로 이미 있다** —
`draw_agent`가 `"… N character(s) elided"`를 그린다([`draw.rs:150-155`](../../crates/rivet-tui/src/draw.rs)).
`answer()`가 지워지면 사라지는 것은 stdout 쪽 사본
([`run.rs:392-398`](../../crates/rivet-cli/src/run.rs))뿐이다. 그래도 그 DoD 항목은 유효하다:
링 버퍼의 **범위**가 run에서 대화로 바뀌므로 상한(8 KB → 64 KB)과 출처(`run.text_dropped` →
`transcript.dropped_chars`)가 이 Phase에서 어차피 움직인다.

**3. "수리·취소 토큰·취소 플래그는 turn마다"는 셋을 나란히 적었는데, 뒤의 둘은 나란히 두면
어긋난다.** 계획서의 위험 요소는 토큰과 플래그를 각각 진단하고 각각 고치라고 말한다. 따로
고치면 "토큰은 갈았는데 플래그는 안 갈았다"가 표현 가능하고, 그 상태는 두 회귀를 **하나씩**
되살린다. `CurrentTurn`이 둘을 한 값에 담는 것이 이 설계가 그 문장에서 벗어난 지점이다 (§2.1-a).

**4. `Intent::Prompt(String)`을 만들지 않는다.** 계획서 4b.1의 이름이고, 이 설계는 대신
`Prompt(String)`이라는 별도 타입을 둔다. 이유는 하나다: 프롬프트가 자기 채널을 타야 하므로
(4b.1이 요구하는 "유실 없는 전달") 펌프는 그 변이를 **절대 보지 않는데**, 그러면 와일드카드 없는
`Reaction::to`([`run.rs:530`](../../crates/rivet-cli/src/run.rs))가 도달 불가능한 팔을 하나 갖게
된다. 그 팔에 쓸 정직한 답이 없다 — `unreachable!()`은 이 저장소가 타입으로 막는 종류의 것이고,
`Reaction::Ignore`는 죽은 변이다. 타입을 나누면 "두 채널이 교차했다"가 표현 불가능해진다.
전달 보장이라는 요구는 그대로 만족된다.

**5. `dropped`는 "이미 세어 둔 수"가 아니다 — 문자열 키 안에 있다.** 계획서는 4b.5가 "이미
세어 둔 수를 그리는 일이라 압축 전략을 기다릴 필요가 없다"고 적었다. 실제 모양은
`DroppedItem { key: "history.turns:3", … }`이고
([`context/mod.rs:222-230`](../../crates/rivet-runtime/src/context/mod.rs)), 기존 테스트조차
`d.key.starts_with("history.turns:")`로 단언한다
([`context/mod.rs:389-396`](../../crates/rivet-runtime/src/context/mod.rs)). UI가 그 수를 읽으려면
문자열을 파싱해야 하고, 그래서 아무도 안 읽었다. 결론은 안 바뀐다(압축 전략은 기다리지 않는다)
그러나 **작업 범위는 계획이 예상한 것보다 한 crate 넓다**: `rivet-core`의 `AssembledContext`에
필드 하나, `rivet-runtime`의 `assemble`·`build_request`·`request`에 배선, 그리고
`AgentEvent::RequestStarted`에 필드 하나 (§2.1-f, §3.1, §3.2).

**6. `### 4b.8` 표에 세 행을 보탠다.** 계획서의 열다섯은 전부 유효하다. 이 설계가 계획이
예상하지 않은 두 가지를 하므로 그날 손봐야 하는 산문이 세 곳 늘어난다.

| 문장 | 지금 | 4b 이후 |
|---|---|---|
| `events.md:133` — `agent.request.started` 행의 필드 목록 | `model, input_tokens_estimate` | `model, input_tokens_estimate, dropped_turns`. §2.1-f가 그 필드를 더한다. **행이 하나 느는 것이 아니라 있던 행이 길어지는 것**이라 "여덟 → 아홉" 행(표의 첫 행)과 별개로 세어야 한다 |
| `config.md:412-423` — 종료 코드 표 | 실행 하나 = run 하나이므로 표가 그대로 읽힌다 | 표의 **여섯** 행(0 · 1 · 2 · 3 · 4 · 130, [`config.md:418-423`](../config.md))은 그대로이고, `--tui`의 대화는 turn이 여럿이며 **프로세스의 코드는 마지막 turn의 것**이라는 문장이 표 아래에 붙는다. 계획서의 `config.md:77-78` 행(한도 축)과 **다른 자리**다 — 그쪽은 "어느 축이 발동했나", 이쪽은 "여럿 중 어느 것이 이기나" |
| `config.md:398` 아래 — `rivet resume --tui` | 문서에 없다 | `resume`이 `--tui`에서 뜻이 하나 는다: 이미 답한 세션은 거절 대신 컴포저로 열리고, `closed` 세션만 거절된다. 계획서의 "닫지 않는 것" 절이 "4b.6이 정하고 `config.md`가 적는다"고 지목한 자리이고, 정한 답이 §2.1-h다 |

**7. 계획서 DoD 10이 이름 지어 요구한 e2e 하나를 쓰지 않는다.**
`a_conversation_does_not_reprint_its_own_transcript`는 `--tui`가 실제로 뜬 터미널을 필요로
하는데, e2e 하네스는 stdout을 파이프로 받으므로
([`support/mod.rs:305-306`](../../crates/rivet-cli/tests/support/mod.rs)) `--tui`가 그 앞에서
거절된다([`run.rs:138-143`](../../crates/rivet-cli/src/run.rs), `tui_refuses_a_pipe`
[`e2e.rs:588`](../../crates/rivet-cli/tests/e2e.rs)). pty 하네스를 들이는 것은 이 Phase의
항목이 아니다 — Phase 3이 남긴 빈칸이고 이 Phase도 닫지 않는다. 무엇이 대신 서는지는 §6.4에
적었다(삭제는 diff에 보이고, 짝인 `the_summary_is_printed_after_the_terminal_comes_back`은
`Restored` 위트니스가 타입으로 강제한다). **여기 올려 두는 이유는 이것도 "계획이 요구한 것을
안 하기로 한" 결정이기 때문이다** — departure 목록만 읽는 사람이 §6.4까지 내려가지 않아도
알아야 한다.

**8. 계획서가 인용한 `file:line`은 이 base에서 전부 맞다.** §6.0 ①·②로 다시 훑었고,
`run.rs:81`·`:273`·`:274`·`:312-319`·`:321`·`:323`·`:337`·`:363`·`:388`·`:452-458`·`:472-478`·
`:491-509`·`:555`·`:602`, `app.rs:78`·`:243-247`·`:248`·`:249-256`·`:263`,
`state.rs:24`·`:183-214`·`:184-187`·`:211-213`·`:352-364`·`:395-397`,
`agent_loop.rs:199-206`·`:215`·`:330`·`:337`·`:389-397`·`:663`·`:762`,
`session_recovery.rs:39-50`·`:175`·`:177-183`·`:249-266`,
`session.rs:338`·`:377-395`, `context/mod.rs:140-146`·`:162-176`·`:221-230`,
`context/history.rs:72-110`, `config.rs:329-332`·`:339-345`·`:689-697`,
`event_flow.rs:77-84`·`:133-157`·`:162-168`·`:169`,
`telemetry-log/src/lib.rs:63-70`·`:73`·`:176-192`·`:193-198`·`:491`·`:496`,
`telemetry-log/src/record.rs:118-162`, `render/jsonl.rs:30-36`, `render/human.rs:86`,
`main.rs:275`·`:320`·`:330-335`, `exit.rs:23-37`, `draw.rs:21`, `doctor.rs:76`,
`panels.rs:162-165`, `events.md:131-138`·`:178`, `security.md:398`·`:401`,
`architecture.md:756-758`, `plugin.md:154-155`·`:174-175`,
`config.md:77-78`·`:185`·`:187`·`:192-195`·`:204-205`·`:207-208`·`:213-214`·`:398`,
`rivet.example.toml:92-93`·`:94`·`:97-100`, `README.md:19`가 전부 가리키는 것을 가리킨다.
어긋난 것은 위 1번 하나이고, 그것도 줄이 아니라 인용문이다.

---

## 9. 구현이 설계에서 벗어난 곳

<!-- 구현 뒤에 채운다. 위의 본문은 나중에 선견지명처럼 보이도록 고치지 않는다. -->

*(구현 전)*
