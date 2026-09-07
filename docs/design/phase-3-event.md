<!-- Phase 3을 위해 승인받으려는 설계. 구현 전에 쓴다.
     §8은 구현이 이 문서에서 갈라진 지점을 나중에 기록하는 자리이고,
     §8 위의 본문은 나중에 선견지명처럼 보이도록 고치지 않는다. -->

# Phase 3 — Event: 설계

> 상태: 구현 완료 (설계 리뷰 2라운드 통과) · 구현 대상:
> [`docs/plan.md`](../plan.md) Phase 3 (3.1–3.5)
> 이 문서는 **결정과 그 근거**를 남긴다. 무엇을 만들었는지는 커밋이, 계약의 현재 모습은
> [`events.md`](../events.md)와 [`plugin.md`](../plugin.md)가 말한다.
>
> 교차 Phase 파급이 있는 열린 질문은 [`architecture.md` §11](../architecture.md#11-열린-질문)로
> 승격한다. §7에 남은 것은 이 Phase 안에서 닫히는 것들이다.

> 범위: `docs/plan.md` §"Phase 3 — Event"의 작업 항목 3.1–3.5와 DoD 6줄.
> 기준 트리: `herdr/phase-3-event` (base `1c6a176`).
> Phase 0에서 확정된 것(Session event ≠ Bus event · 등록은 소유권 추적 · 언로드는 소유자별
> `JoinHandle` abort · `register_subscriber`는 등록이 곧 attach)은 전제이지 논의 대상이 아니다.

---

## 1. 문제

버스는 이미 있고 lossy하도록 잘 지어져 있지만, 그 위에 아무도 서 있지 않다. 다섯 군데가
비어 있다. 첫째, [`events.md`](../events.md) §3이 세는 **28개 토픽 중 18개만 실제 발행
지점에 붙어 있고** `runtime.started`·`runtime.shutting_down`은 `registry.rs`의 테스트
안에서만 만들어지며, 아직 안 오는 8개(Phase 4의 `tool.policy.evaluated`·approval 둘,
Phase 5의 `job.*` 다섯)와 "그냥 빠뜨린 것"을 구분해 주는 장치가 없다. 둘째, **배달 경로가
grant를 읽지 않는다** — `BroadcastBus::attach`는 `EventSubscriber::topics()`로 거르는데
그건 구독자 **자신의** 선호이고 기본값인 빈 목록을 `topic_matches`가 "전부"로 읽으므로,
`events_subscribe`를 아예 선언하지 않은 plugin도 구독할 수 있고 `["tool."]`만 선언한
plugin도 모델 출력 전문을 나르는 `agent.text.delta`를 받는다
([`architecture.md` §11-10](../architecture.md), [`security.md` §8](../security.md),
[`plugin.md` §2.5](../plugin.md)가 전부 같은 자리를 가리키고 있다). 셋째, **구독하는
plugin이 하나도 없어서** `attach_subscriber`도, Phase 0이 넣어 둔 소유자별 `JoinHandle`
abort도, 구독자에 대한 롤백도 실제 plugin으로 한 번도 돌아 본 적이 없다. 넷째, `--jsonl`은
Phase 1부터 있지만 **`catalog::load`가 반환된 뒤에** 붙으므로 `runtime.started`와 plugin
lifecycle 전부가 아무도 안 듣는 사이에 지나가고, 스트림 꼬리는 `sleep(20 ms)` 다음
`abort()`로 잘린다 — 시계에 달린 스트림은 관찰 가능성이 아니라 복권이다. 다섯째,
`rivet-tui`는 모듈 doc 세 줄이고 그 `Cargo.toml`은 **이미 `rivet-runtime`에 의존한다** —
DoD 1이 금지하는 바로 그것이다. Phase 3은 이 다섯을 닫는다.

---

## 2. 접근

### 2.1 모양

```text
       rivet-cli (host)                        plugins/telemetry-log
   ┌──────────────────────────┐                ┌──────────────────┐
   │ 1 BroadcastBus::new()    │                │ EventSubscriber  │
   │ 2 bus.observe(renderer)  │◀── 관측자 먼저   │ topics() = 설정   │
   │ 3 lifecycle::started()   │                └────────┬─────────┘
   │ 4 catalog::load(cfg,bus) │                         │ register_subscriber
   │ 5 AgentLoop::run()       │                         ▼
   │ 6 lifecycle::            │              GuardedRegistry (rivet-plugin)
   │     shutting_down()      │        capabilities 검사 + grant ⊓ topics() 검사
   │ 7 loader.unload_all()    │                  + ScopedSubscriber 로 감싸기
   │ 8 observer.drain_within()│                         │
   └───────────┬──────────────┘                         ▼
               │                            ScopedRegistry::register_subscriber
               ▼                                        │
        BroadcastBus  ◀───────── publish (동기·무오류) ──┴── attach_subscriber
          │  │  │                                            (소유자별 JoinHandle)
          │  │  └── attach()  : plugin 펌프. 랙은 SubscriberLagged 로 보고.
          │  └───── observe() : 호스트 펌프. 같은 랙 보고 + drain 가능.
          ▼
     rivet-tui  (rivet-core 만 본다: EventEnvelope · Event · EventSubscriber)
       AppState::apply  →  draw(TestBackend | CrosstermBackend)  →  Intent
```

설계를 지탱하는 결정 다섯.

**(a) `runtime.*`는 호스트가 발행하고, 관측자는 관측 대상보다 먼저 붙는다.**
`rivet_runtime::lifecycle::{started, shutting_down}` 두 함수가 토픽과 payload 모양을 한
군데 모으고, `run.rs`가 버스를 만들어 **관측자를 붙인 다음** `catalog::load`를 부른다.
그래서 `--jsonl` 스트림은 `runtime.started`로 시작해 `plugin.discovered`·`plugin.loaded`를
싣고 `runtime.shutting_down`·`plugin.unloaded`로 끝난다. 지금은 그 절반이 청중 없는
방에서 일어난다.

**(b) 토픽 필터는 grant와 선호의 meet이고, 그 meet은 이미 있는 `Permission::meet`이다.**
`EventsSubscribe`의 meet은 문자열 교집합이 아니라 "한쪽이 다른 쪽의 접두사여야 하고 그때
긴 쪽이 답"이며(`capability.rs::meet_prefixes`), 그건 `manifest ∩ profile`이 이미 쓰는
함수다. 배달 시점에 같은 함수를 다시 쓰면 **판정 지점이 둘로 갈라지지 않는다.** 새 헬퍼를
쓰면 언젠가 한쪽만 고쳐진다 — `TopicScope`가 태어난 이유가 정확히 그것이었다
(`allows`, `plugin show`의 두 분기, 세 커밋에서 세 번).

**(c) 빈 meet은 에러다. 조용한 통과가 아니다.**
빈 목록이 `topic_matches`에서 "전부"로 읽히므로, grant와 겹치지 않는 `topics()`를 그대로
넘기면 **권한이 0인 구독자가 전부를 받는다.** 정확히 뒤집힌다. `TopicScope::new`가 빈
목록을 거부하는 것과 같은 이유이고, 여기서는 그 거부가 `register_subscriber`의 `Err`가
된다.

**(d) 강제는 `GuardedRegistry`에서 한다.** grant(`ctx.permissions`)가 이미 거기 있고,
`capabilities` 검사도 이미 거기 있다. `Registry::scoped`에 grant 인자를 더하는 것은 모든
embedder에 대해 Phase 0 계약을 바꾸는 일이고, Phase 2가 declared-kinds에 대해 같은 이유로
거절했다. 감싼 구독자(`ScopedSubscriber`)의 `topics()`는 등록 시점에 고정되므로 plugin이
나중에 넓힐 수 없다 — [`architecture.md` §11-14](../architecture.md)가 `tool.spec()`에
대해 지적한 "창구는 존재를 닫지 내용을 닫지 않는다"의 구독자 판이 여기서는 열리지 않는다.

**(e) TUI는 `rivet-core`만 본다.** 상태는 `AppState::apply(&EventEnvelope)`라는 **순수
fold**, 그리기는 `draw(frame, &AppState)`, 터미널은 그 위의 얇은 층. 그래서 패널 전체를
손으로 만든 envelope 목록으로 테스트할 수 있고, `rivet-runtime`은 `Cargo.toml`에서
**빠진다** — DoD 1이 lint가 아니라 컴파일 성질이 된다. TUI는 런타임에 손을 뻗는 대신
`Intent`를 밖으로 내보내고, 그것을 취소 토큰에 매핑하는 것은 호스트다. 구독자는 관찰하고
interceptor가 판단한다는 규칙([`architecture.md` §5.3](../architecture.md))의 UI판이다.

**`rivet-core` 변경은 함수 다섯 개뿐이다.** `ToolEvent`·`JobEvent`·`PluginEvent`·
`RuntimeEvent`에 `one_of_each()`(`AgentEvent`에 이미 있는 것과 같은 것)와 그것들을 모으는
`Event::one_of_each()`. 타입도, 계약도, wire 표현도 바뀌지 않는다. 이 다섯이 사는 이유는
`Event::topic`의 doc 주석이 스스로 적어 둔 것이다 — "패밀리가 새로 생기면 좁힌 프로파일이
그것을 못 받는데 tripwire가 없다. **이 주석이 tripwire다.**" 주석이 하던 일을 테스트가
하게 만드는 것이 Phase 3의 몫이다.

### 2.2 기각한 대안

| 대안 | 기각 이유 |
|---|---|
| **TUI가 `bus.subscribe_raw()`를 직접 받는다** ([`events.md`](../events.md) §7의 예시 그대로) | `subscribe_raw`는 `BroadcastBus`의 메서드이고 `BroadcastBus`는 런타임 타입이다. 그 예시는 바로 아래에서 "TUI는 런타임 타입을 하나도 import 하지 않는다"고 쓰고 있으므로 **문서가 자기와 모순**이다. 예시를 고치는 것이 Phase 3의 일이다. |
| **`rivet-core`에 `EventFeed` 트레이트를 새로 판다** (`Feed::Event` / `Feed::Lagged`를 yield) | 배달 방식이 둘이 된다. `EventSubscriber`가 이미 "core에 있는, 런타임을 모르는 소비 계약"이고 TUI는 그것을 구현하면 된다. 랙은 `SubscriberLagged`로 이미 버스에 실려 오므로 별도 채널이 필요 없다. |
| **`Registry::scoped(owner, grant)`로 grant를 레지스트리에 넣는다** | Phase 0 계약을 모든 embedder에 대해 바꾼다. Phase 2가 declared-kinds에 대해 같은 이유로 거절했고, grant가 사는 곳은 manifest가 사는 곳(로더)이다. |
| **`BroadcastBus::attach`가 grant를 읽게 한다** | 버스가 permission과 plugin을 알게 된다. 그러면 [`§11-10`](../architecture.md)이 명시적으로 면제한 **호스트 자신의 관측자**(`--jsonl`, TUI)도 grant를 통과해야 하는데, 그 둘을 제약하는 것은 프로파일이 아니라 CLI를 실행한 사람이다. |
| **grant와 겹치지 않는 `topics()`를 빈 목록으로 넘긴다** | 빈 목록은 "전부"다. 권한 0인 구독자가 모든 토픽을 받는다. 정반대로 실패한다. |
| **`agent.text.delta`를 `telemetry.log`의 기본 구독에 넣는다** | `developer`의 grant는 전체이므로 프로파일이 막아 주지 않는다. 대화 전문이 기본값으로 로그에 나가는 것은 plugin 자신이 막아야 한다 — 기본 목록에서 뺀다. |
| **`telemetry.log`가 파일에 직접 쓴다** | `fs_write`는 `developer`·`ci`만 받으므로 다른 프로파일에서 관용구 (1)이 발동해 plugin이 아예 안 뜬다. 게다가 telemetry를 워크스페이스에 쓰는 것 자체가 틀렸다. `tracing`으로 내보내면 sink와 필터는 운영자 몫이 되고 권한이 필요 없다. |
| **`runtime.shutting_down`을 펌프 종료 sentinel로 쓴다** | 그 뒤에 오는 `plugin.unloaded`를 아무도 못 본다. sentinel 대신 **채널이 빌 때까지** 배달하고 끝낸다 — 발행은 동기이므로 "빔"은 "지금까지 발행된 것을 다 줬다"와 같은 말이다. |
| **`sleep(20 ms); abort()`를 유지한다** | 스트림의 꼬리가 스케줄러 운에 달린다. CI 소비자에게 이건 관찰 가능성이 아니다. |
| **Job 패널을 Phase 5까지 안 만든다** | 3.5가 미완으로 남고, Phase 5가 이벤트 대신 런타임 타입을 들여다볼 유혹이 생긴다 — DoD 1을 나중에 깨는 가장 그럴듯한 경로다. 패널은 지금 만들고 손으로 만든 `job.*` envelope로 테스트한다. Phase 5인 것은 **생산자**뿐이다. |
| **TUI를 기본 출력으로 만든다** | Phase 1 e2e 전부가 human 렌더러의 stdout/stderr 분리를 전제한다. 그리고 승인 프롬프트가 없는 TUI는 Phase 4.4보다 이르다. Phase 3은 `--tui` opt-in. |
| **plugin id를 `telemetry.log`로 쓴다** (plan.md의 표기 그대로) | 아무도 소유하지 않는 `telemetry` namespace가 생긴다. 이 저장소가 싣는 plugin은 전부 `rivet.`이다. id는 `rivet.telemetry-log`, crate는 `plugins/telemetry-log`. |
| **`[plugins."rivet.telemetry-log"]`에 `enabled` 키를 둬서 설정 없이는 구독자를 등록 안 하게 한다** | 켜는 스위치가 둘이 된다. 그리고 로드는 됐는데 아무것도 등록하지 않는 plugin은 `rivet plugin list`에 하는 거짓말이다 — Phase 2가 `tool-shell` 스텁을 거절한 것과 같은 이유다. 대신 **선택**에서 뺀다(§3.6 `default_selection`). |
| **`PluginSelection::All`을 "카탈로그 전부"로 그대로 두고 telemetry를 카탈로그에 넣는다** | 설정 파일 없는 트리에서 telemetry가 켜진다 — §7-8이 원치 않는다고 적은 상태가 §3.6에서 실제로 일어난다(리뷰 1 blocking). `All`이 존재하는 이유는 `config.rs`가 스스로 적어 뒀다: "`rivet "explain this repo"`가 `rivet.toml` 없는 디렉터리에서 동작하게". 에이전트를 돌리는 것과 관측 사이드카는 다르다. |
| **`GuardedRegistry`에 `effective`만 넘긴다** | "매니페스트가 요청하지 않았다"와 "프로파일이 깎았다"가 `effective` 안에서 구별되지 않는다 — 둘 다 `EventsSubscribe`의 부재다. 그런데 운영자가 할 일은 다르다(매니페스트를 고쳐라 / `--profile`을 바꿔라). 로더가 이미 들고 있는 `record.denied`를 함께 넘긴다(§4.4 `Grant`). |
| **effective set의 첫 `EventsSubscribe`를 고른다** | 매니페스트가 `events_subscribe`를 두 줄 적으면 두 개가 남는다(`PermissionSet`의 dedup은 값이 같을 때만 접는다). 첫 번째를 고르는 것은 조용히 좁히거나 넓힌다. **join**한다 — plugin은 둘 다 가진 것이 맞다. |
| **grant가 좁혀 버린 `topics`를 조용히 지운다** | 운영자가 명시적으로 적은 것이 말없이 사라진다. 기본 목록은 *선호*라 좁혀도 되지만 설정 파일에 적힌 것은 *약속*이다. 설정된 접두사가 하나라도 meet에서 사라지면 로드를 거부한다(§5-7). |
| **`load`/`unload`에 타임아웃을 넣는다** | [`architecture.md` §11-15](../architecture.md)가 열어 둔 것은 숫자가 아니라 초과 시 남길 상태다. §5 끝을 보라 — 이번에는 **고치지 않고 보이게** 만든다. |

---

## 3. 구체적 변경

### 3.1 `crates/rivet-core` — 다섯 함수 (3.1의 표본 공급)

- **`src/event.rs`** — `ToolEvent::one_of_each()`, `JobEvent::one_of_each()`,
  `PluginEvent::one_of_each()`, `RuntimeEvent::one_of_each()`를 `AgentEvent`의 것과 같은
  모양으로 추가하고, 다섯을 모으는 `Event::one_of_each() -> Vec<Event>`.
  `Event::topic`과 `AgentEvent::topic`의 doc 주석에서 "이 주석이 tripwire다" 문장을
  지우고 그 자리에 테스트 이름을 적는다.

**`Vec<Event>`이지 `Vec<&'static str>`이 아니다.** 토픽 문자열을 돌려주면 소비자가
`&str`로 판정하게 되고, 그 순간 오늘 있는 컴파일 타임 tripwire가 런타임 실패로 내려앉는다
— `every_agent_topic_is_granted_or_deliberately_withheld`(`config.rs:572-584`)가 잡는 것은
와일드카드 없는 `match`이지 문자열 비교가 아니고, 그 테스트의 doc이 "a new variant fails to
compile *here*"라고 스스로 적는다. 값을 돌려주면 소비자가 계속 `match`할 수 있고, Phase 3은
그 `match`를 패밀리 층까지 넓히는 것으로 [`§11-10`](../architecture.md)이 "silent"라고 적어
둔 구멍을 **컴파일 타임에** 닫는다.

`one_of_each()` 자체는 `vec![]` 리터럴이므로 변형을 더해도 컴파일이 깨지지 않는다. 그것을
강제하는 것은 이 함수가 아니라 이 함수를 **소비하는 두 개의 와일드카드 없는 `match`**다
(§6.2). 순서는 이렇다: 변형을 더한다 → 그 `match`가 컴파일에 실패한다 → arm을 더한다 →
그 arm이 기대 목록에 없다 → 테스트가 실패한다 → `one_of_each`를 고친다. core 쪽에서는
`every_sample_has_a_distinct_topic`이 복붙 중복만 잡는다. 이 이상은 `one_of_each`가
값 목록인 한 얻을 수 없고, 그렇다고 적지 않는다.

이게 전부다. 타입·계약·wire 표현은 손대지 않는다.

### 3.2 `crates/rivet-runtime` — 발행 지점과 호스트 펌프 (3.1)

- **`src/lifecycle.rs` (신규)** — `started(&dyn EventBus, version: &str)`와
  `shutting_down(&dyn EventBus, reason: &str)`. 모듈 doc이 "`runtime.*`은 호스트의 것이다:
  런타임을 시작한 쪽만 언제 시작했는지 안다"를 적는다.
- **`src/bus.rs`** — `attach`의 몸통을 `pump(rx, subscriber, bus, stop: Option<..>)`으로
  빼고, `observe(subscriber) -> Observer`를 더한다. 두 경로가 같은 랙 보고를 공유한다.
  `Observer::drain_within(budget)`은 `stop`을 켜고 펌프가 **채널을 비울 때까지** 기다린 뒤
  `Drained::{Complete, Truncated}`를 돌려준다. `Observer`의 `Drop`은 abort이므로 drain
  없이 버리는 것도 안전하다.
- **`src/registry.rs`** — 변경 없음. `attach_subscriber`와 소유자별 `JoinHandle` abort는
  Phase 0에서 이미 맞다. Phase 3이 하는 일은 **그것이 실제 plugin에 대해 동작함을 증명**
  하는 것이다(DoD 4·5).
- **`tests/support/mod.rs`** — `Harness::with_bus_capacity(n)`, 버스 토픽을 순서대로 모으는
  `Harness::recorder()`, `ctx.host.progress`를 부르는 `ProgressTool`, 델타를 여러 프레임으로
  쪼개는 `sse_text_in_chunks(n)`.
- **`tests/event_flow.rs` (신규)** — 3.1의 완전성, DoD 2, DoD 3.

### 3.3 `crates/rivet-plugin` — 등록과 토픽 필터 (3.2)

**왜 `rivet-runtime`이 아닌가.** `plan.md`의 3.2 행은 crate를 `rivet-runtime`으로 적고
[`architecture.md` §11-10](../architecture.md)은 이음매를 `attach_subscriber`라고 적는다.
등록 경로는 그대로 런타임에 남는다 — 바뀌는 것은 **강제가 어디서 일어나는가**뿐이다.
grant(`ctx.permissions`)와 `capabilities` 검사가 이미 guard에 있고, `Registry::scoped`에
grant 인자를 더하는 것은 모든 embedder에 대해 Phase 0 계약을 바꾸는 일이다(Phase 2가
declared-kinds에 대해 같은 이유로 거절했다). 표와 §11-10 문장 양쪽에 대한 **의도적 이탈**
이므로 여기 적어 둔다.

- **`src/subscriber.rs` (신규)** — `Grant`, `effective_topics(&Grant, wanted)`,
  `ScopedSubscriber`. 가운데 함수는 grant 안의 **모든** `EventsSubscribe`를 join한 뒤
  `Permission::meet`을 그대로 태운다(§4.4). `ScopedSubscriber`는 `EventSubscriber`를 감싸
  `topics()`만 바꾸고 `name()`·`on_event`는 위임한다. 이름을 위임하는 것이 중요하다 —
  레지스트리 키와 랙 보고의 `subscriber` 필드가 plugin이 고른 이름 그대로 남는다.
- **`src/guard.rs`** — `GuardedRegistry::new(inner, plugin_id, declared, grant: Grant)`.
  `register_subscriber`가 ① 슬롯 선언(`event_subscriber`, 기존) ② `events_subscribe` 보유
  ③ grant ⊓ `topics()`가 비지 않음, 셋을 차례로 확인한 뒤 감싼 구독자를 위임한다. 세 실패의
  메시지는 서로 구분되고, ②는 **`grant.denied`를 보고 다시 둘로 갈린다**(§5-4).
- **`src/loader.rs`** — `GuardedRegistry::new` 호출부에서
  `Grant { effective: record.effective.clone(), denied: record.denied.clone() }`을 만들어
  넘긴다. 둘 다 `validate`가 이미 채워 둔 필드다(`loader.rs:194-201`). 그 외 변경 없다 —
  로더는 이미 아는 것을 넘길 뿐, 새로 계산하지 않는다.
- **`src/lib.rs`** — `Grant`·`ScopedSubscriber` 재수출.
- **`tests/subscriber.rs` (신규)** — DoD 4·5와 필터 거부 다섯 종류.

### 3.4 `plugins/telemetry-log` (신규 crate) — 3.3

`rivet.telemetry-log`. 카탈로그 규약을 그대로 따른다(`rivet-plugin.toml` +
`MANIFEST_TOML` + `PLUGIN_ID` + 매니페스트 정직성 테스트).

- **`Cargo.toml`** — deps `rivet-core`, `async-trait`, `serde`, `serde_json`, `tracing`;
  dev-deps `rivet-plugin`, `rivet-runtime`, `tokio(test-util)`, `tracing-subscriber`.
  **`rivet-runtime`이 dev-dep인 것에 주의** — plugin 본체는 core만 본다.
- **`rivet-plugin.toml`** — §4.1.
- **`src/lib.rs`** — `TelemetryLogPlugin`(=`Plugin`)과 `LogSubscriber`(=`EventSubscriber`).
  `load`는 설정을 읽어 구독자 하나를 등록하고 끝난다. 네트워크도, 파일도, 기다림도 없다.
  `unload`는 no-op이다 — 펌프는 레지스트리가 abort한다(§5 끝).
  **설정된 것과 기본값을 다르게 다룬다**: 기본 토픽 목록은 *선호*라 grant가 좁히면 좁혀진
  채로 등록하고, `topics`나 `include_conversation = true`처럼 운영자가 **적어 넣은** 것이
  grant에서 사라지면 `load`가 `Err`로 실패하며 사라진 접두사를 이름으로 댄다(관용구 (1)).
  `meet_prefixes`는 맞는 쌍마다 결과를 미는 **합집합**이므로(`capability.rs:380`), 이
  규칙이 없으면 `include_conversation = true` + `readonly`는 "거부"가 아니라 "조용히 덜
  로그함"이 된다 — telemetry가 말없이 과소 보고하는 것은 이 Phase가 존재하는 이유의
  반대편이다.
- **`src/record.rs`** — envelope → `tracing` 이벤트. 필드는 `topic`·`event_id`·`session`·
  `run`·`at` 고정에 payload에서 뽑은 소수의 필드(`tool`·`call_id`·`turn`·`model`·
  `duration_ms`·`is_error`·`dropped` 등). payload 전체를 JSON으로 붓지 않는다 — 그러면
  `--jsonl`의 열등한 사본이 될 뿐이고, 구조화 로그의 값은 필드가 **고정**되어 있다는 데
  있다.
- **`tests/`** — §6.

### 3.5 `crates/rivet-tui` — 기본 TUI (3.5)

- **`Cargo.toml`** — **`rivet-runtime` 삭제**(DoD 1). 남는 것은 `rivet-core`,
  `async-trait`, `tokio`, `tokio-util`, `ratatui`, `crossterm`; dev-dep `toml`.
  `serde`/`serde_json`은 쓰지 않으므로 뺀다. `tokio-util`은 `render_loop`이 받는
  `CancellationToken` 때문이다(§4.5) — 호스트가 취소에 쓰는 것과 **같은 타입**이어야 하고,
  그것은 서드파티 crate이지 런타임 타입이 아니다. `toml`은 DoD 1 테스트가 자기
  `Cargo.toml`을 **파싱**하기 때문이다: 문자열 검색이면 주석에 걸려 오탐하고 워크스페이스
  상속 표기(`rivet-runtime.workspace = true`)에서 놓칠 수 있다.
- **`src/lib.rs`** — 모듈 doc이 규칙을 적는다: 이 crate는 이벤트가 나르는 것만 보여준다.
  상태바가 원하는데 어떤 이벤트도 안 나르는 값이 있으면, 답은 import가 아니라 **이벤트를
  하나 더 만드는 것**이다(§7-2).
- **`src/state.rs`** — `AppState`, `AppState::apply(&EventEnvelope)`(순수), `Panel`,
  `RunView`, `JobView`, `StatusView`. 텍스트와 tool 목록은 상한이 있는 링 버퍼다(§5-13).
- **`src/draw.rs`** — `draw(frame, &AppState)`. Job 패널 · Agent 패널 · 상태바 세 영역.
- **`src/app.rs`** — `Tui`가 `EventSubscriber`를 구현해 `Mutex<AppState>`에 fold하고,
  `Tui::render_loop(cancel) -> impl Future`가 틱마다 그린다. 펌프와 그리기가 서로를 막지
  않는다. 키 입력은 `Intent`가 되어 `mpsc::Sender<Intent>`로 나간다.
- **`src/terminal.rs`** — `TerminalGuard`. raw mode 진입/복구를 `Drop`으로 묶고 패닉 훅을
  건다. (`Sandbox`가 `Drop`을 못 쓰는 이유는 await가 필요해서였다. 여기서는 await가
  없으므로 `Drop`이 맞는 도구다.)
- **`tests/independence.rs` · `tests/panels.rs` (신규)** — §6.

### 3.6 `crates/rivet-cli` — 스트림과 배선 (3.4)

- **`src/main.rs`** — `--tui` 추가(`--jsonl`·`--headless`와 상호 배타).
  `Output::{Human, Jsonl, Tui}`. `tracing_subscriber` 초기화에 두 가지를 더한다: 기본
  EnvFilter가 `warn,rivet_telemetry_log=info`가 되어 telemetry plugin이 **켜져 있으면
  실제로 보이고**(`RUST_LOG`는 여전히 통째로 덮어쓴다), `RIVET_LOG_FORMAT=json`이면
  `.json()` 레이어를 쓴다 — 그게 없으면 "구조화 로그"의 절반만 참이다. 이 지시어는 plugin이
  기본 선택에서 빠지므로(바로 아래) 아무도 안 켠 실행에서는 **가리킬 대상이 없다** — 기본
  경로의 stderr는 오늘과 같다.
- **`src/catalog.rs`** — `load(config, bus: BroadcastBus)`가 버스를 **받는다**. 버스를
  누가 만드는지가 곧 관측자를 언제 붙일 수 있는지이므로, 그 결정은 호출자 몫이다.
  `inspect`는 **그대로 둔다** — 아무것도 구성하지 않고 `plugin list`/`plugin show`만 쓰며
  §7-4가 정한 대로 그 버스에 붙는 관측자가 없으므로, 시그니처를 바꾸는 근거가 없다.
  `sources()`에 `rivet-telemetry-log` 한 줄, 그리고:

  ```rust
  /// 비어 있거나 없는 `[plugins].enabled`가 뜻하는 id 목록.
  ///
  /// "카탈로그 전부"가 아니다. `PluginSelection::All`이 존재하는 이유는 `config.rs`가
  /// 스스로 적어 뒀다 — `rivet "explain this repo"`가 `rivet.toml` 없는 디렉터리에서
  /// 동작하게 하는 것. 에이전트를 돌리는 데 필요한 것과 관측 사이드카는 다르다.
  ///
  /// 이 목록 밖의 plugin도 **discover되고**, `rivet plugin list`에 나오고,
  /// `enabled`가 이름을 대는 순간 로드된다. Phase 2의 규칙("id는 로드되거나 오타다")은
  /// 그대로다 — 이 목록이 정하는 것은 *기본 선택*이지 *제공 여부*가 아니다.
  pub fn default_selection() -> Vec<PluginId>;
  ```

  `select`의 `PluginSelection::All` 분기가 `known.clone()`(`catalog.rs:145`) 대신 이것을
  쓴다. `rivet plugin list`에 `DEFAULT` 열이 붙어 어느 것이 기본인지 한눈에 보인다.
  telemetry 계열은 MVP+에 `telemetry-otel`·`telemetry-prometheus`가 더 온다고 plan.md의
  MVP 경계 표가 말하므로, 이 구분은 이 plugin 하나를 위한 것이 아니다.
- **`src/render/mod.rs`** — `Output`이 여기로 오고, `attach(&bus, output) -> Observers`가
  세 모드를 만든다. `Observers::drain_within`이 §3.2의 것을 감싼다.
- **`src/run.rs`** — `drive`의 순서를 §2.1대로 바꾼다. `sleep(20 ms); abort()`가 사라지고
  `drain_within(2 s)`가 들어온다. `Drained::Truncated`면 stderr에 한 줄 경고. `--tui`면
  `TerminalGuard`를 잡고 `render_loop`을 띄우고 `Intent::{Quit, Cancel}`을 취소 토큰에
  매핑한다. 요약 출력은 터미널 복구 **뒤**에 한다.
- **`src/config.rs`** — `Profile::subscribable_topics`는 그대로. 그 옆의
  `every_agent_topic_is_granted_or_deliberately_withheld`를 `Event::one_of_each()` 위에서
  **와일드카드 없는 바깥 `match`**(패밀리)와 **와일드카드 없는 안쪽 `match`**(변형) 두 층으로
  일반화한다. 오늘 그 테스트가 가진 컴파일 타임 성질을 잃지 않으면서 §11-10이 "silent"라고
  적어 둔 패밀리 구멍까지 같은 층에서 닫는 것이 요점이다 — `&str` 키로 판정하면 그 성질이
  런타임으로 내려앉는다(§3.1). `[plugins."rivet.telemetry-log"]` 설정 키 문서화.
- **`tests/e2e.rs`** — 기존 `jsonl_output_is_one_parseable_event_per_line`은 그대로 두고
  §6의 것들을 더한다. `with_no_config_file_every_plugin_the_build_provides_is_loaded`
  (`e2e.rs:209`)는 본문이 에이전트 스택의 등록만 단언하므로 **통과 상태 그대로**지만 이름이
  거짓이 된다 — `..._every_plugin_the_default_selection_names_is_loaded`로 고치고
  telemetry가 discover는 됐고 로드는 안 됐다는 단언을 더한다.

### 3.7 워크스페이스 · 문서 · 예제

- **루트 `Cargo.toml`** — `plugins/telemetry-log`를 `[workspace] members`에,
  `rivet-telemetry-log = { path = ..., version = "0.1.0" }`를 `[workspace.dependencies]`에.
  두 목록 모두 오늘 일곱 plugin을 명시적으로 연다.
- **`docs/design/phase-3-event.md`** — **이 문서 자체**. Phase 2가 그랬듯 저장소에 남는다.
  §8은 구현 뒤에 채운다.
- **`docs/plan.md`** — Phase 3 DoD 6줄에 각각 증명 테스트 이름, 진행 추적 표 갱신.
  검증하지 못한 것은 이유와 함께 체크하지 않는다.
- **`docs/events.md`** — §7의 TUI 예시를 `subscribe_raw`에서 `EventSubscriber` 구현으로
  고친다(§2.2 첫 행). §5에 호스트 관측자도 같은 랙 보고를 받는다는 문장. §3 토픽 목록에
  Phase 열 추가(§4.7).
- **`docs/plugin.md`** — §2.5의 "⚠ 이 scope는 강제가 아니다" 경고 블록을 **강제된 뒤의
  규칙**으로 교체: 빈 `topics()`는 grant의 목록이 되고, `events_subscribe` 없는 plugin은
  구독자를 등록할 수 없고, grant와 겹치지 않는 `topics()`는 `Err`다. §1 표의 `Command`
  행 각주(§7-5).
- **`docs/security.md`** — §8의 같은 경고 블록 교체.
- **`docs/architecture.md`** — §11-10의 "셋째로, 이 scope는 아직 강제되지 않는다" 항목을
  **닫는다**(닫힌 날짜와 테스트 이름을 남긴다). §11-15에 Phase 3이 더한 관측 가능성 한
  문단(고쳐지지 않았고 이제 보인다).
- **`docs/config.md`** — `[plugins."rivet.telemetry-log"]` 표, `RIVET_LOG_FORMAT`,
  기본 EnvFilter가 바뀐 것.
- **`rivet.example.toml`** — `rivet.telemetry-log`를 주석 처리된 예로 넣고, 그 옆에
  "카탈로그에 있지만 기본 선택에는 없다"는 한 줄. 명시적 `enabled` 목록을 가진 사용자에게만
  닿는 파일이므로 이것만으로는 기본 경로를 막지 못한다 — 막는 것은 `default_selection`이다.
- **`examples/minimal-agent`** — 변경 없음. 버스를 직접 쓰는 최소 예제이고 Phase 3이
  더하는 것 중 그 예제가 필요한 것은 없다.

---

## 4. 데이터·인터페이스 모양

### 4.1 `plugins/telemetry-log/rivet-plugin.toml`

```toml
[plugin]
id           = "rivet.telemetry-log"
name         = "Telemetry log"
version      = "0.1.0"
abi_version  = "0.1"
description  = "Structured logs from the event bus, through `tracing`. Observation only."
capabilities = ["event_subscriber"]

# scope 없음 = 모든 토픽을 *요청*한다. 실제로 받는 것은 프로파일과 meet한 결과이고,
# 대화 전문(`agent.text.`)은 grant가 줘도 이 plugin이 기본 목록에서 스스로 뺀다.
[[permissions]]
permission = "events_subscribe"
```

`fs_write`도 `network_http`도 요청하지 않는다. 로그의 목적지는 호스트의 `tracing` sink이고,
그것을 고르는 것은 운영자다.

### 4.2 `[plugins."rivet.telemetry-log"]`

```toml
[plugins."rivet.telemetry-log"]
# 구독 접두사. 생략하면 아래 기본값.
topics  = ["agent.run.", "agent.turn.", "agent.request.", "tool.", "plugin.", "runtime."]
level   = "info"        # trace|debug|info|warn
# 모델 출력 전문을 로그에 넣는다. 기본은 false이고, false면 `agent.text.delta`는
# 구독하지도 않는다 -- 길이만 세는 것도 안 한다. 세려면 받아야 하고, 받으면 남는다.
include_conversation = false
```

기본 `topics`는 `agent.text.`를 **빼고 열거**한다. 접두사는 부정을 표현할 수 없으므로
`Profile::subscribable_topics`가 쓰는 것과 같은 방식이고, 같은 대가를 진다 — `agent.` 밑에
토픽이 새로 생기면 누가 여기 추가하기 전까지 로그에 안 나온다. 닫히는 쪽으로 실패한다.
기본 여섯 개는 좁은 프로파일 셋의 grant 안에도 전부 들어 있으므로, 아무것도 설정하지 않은
telemetry는 어느 프로파일에서든 그대로 로드된다(`the_default_topics_survive_every_shipped_profile`).

**설정된 것과 기본값은 다르게 취급된다.** 이 표가 그 규칙이다.

| 무엇을 적었나 | grant가 그것을 좁히면 |
|---|---|
| 아무것도 (기본 목록) | 좁혀진 채로 등록한다. 기본 목록은 plugin의 *선호*다. |
| `topics = [...]` | **`load` 실패.** 사라진 접두사와 활성 프로파일을 이름으로 댄다. |
| `include_conversation = true` | **`load` 실패** — `agent.text.`를 안 주는 프로파일에서. 이 키는 "대화를 로그하라"는 명시적 지시이고, 답이 "아니오"라면 조용히 덜 로그하는 것이 아니라 말해야 한다. |

`meet_prefixes`가 맞는 쌍마다 결과를 미는 **합집합**이기 때문에 이 규칙이 필요하다
(`capability.rs:380`). 기본 여섯 개 + `agent.text.`를 `readonly` grant와 meet하면 결과는
비지 않는다 — 여섯 개가 살아남고 `agent.text.`만 사라진다. 즉 guard의 "빈 meet 거부"는 이
경우를 **잡지 못하고**, 잡는 것은 plugin 자신이다. 이 문단이 없으면 §5-7과
`a_readonly_profile_refuses_a_telemetry_plugin_that_was_told_to_log_the_conversation`은
거짓이다 (리뷰 1 non-blocking).

`include_conversation = false`(기본)이면 `agent.text.`를 **구독하지도 않는다.** 길이만
세는 것도 안 한다 — 세려면 받아야 하고, 받으면 어딘가에 남는다.

### 4.3 `rivet-runtime`의 새 공개 API

```rust
// bus.rs
impl BroadcastBus {
    /// plugin 펌프. 버스가 닫힐 때까지 돈다. (기존)
    pub fn attach(&self, subscriber: Arc<dyn EventSubscriber>) -> tokio::task::JoinHandle<()>;

    /// 호스트 펌프. `attach`와 같은 배달·같은 랙 보고에 **끝낼 수 있는** 손잡이가 붙는다.
    /// 호스트 자신의 소비자(`--jsonl`, TUI)는 plugin이 아니므로 grant를 지나지 않는다
    /// (`architecture.md` §11-10).
    pub fn observe(&self, subscriber: Arc<dyn EventSubscriber>) -> Observer;
}

#[derive(Debug)]
pub struct Observer { /* stop: CancellationToken, handle: Option<JoinHandle<()>> */ }

impl Observer {
    /// 지금까지 발행된 것을 전부 배달하고 끝낸다. 발행이 끝난 뒤에 부른다.
    ///
    /// `publish`는 동기이므로 "채널이 비었다"는 "지금까지 발행된 것을 다 줬다"와 같은
    /// 말이다. sentinel 이벤트를 쓰지 않는 이유는 그 뒤에 오는 `plugin.unloaded`를
    /// 잘라 먹기 때문이다.
    pub async fn drain_within(self, budget: Duration) -> Drained;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drained { Complete, Truncated { budget_ms: u64 } }

// lifecycle.rs
pub fn started(bus: &dyn EventBus, version: &str);
pub fn shutting_down(bus: &dyn EventBus, reason: &str);
```

### 4.4 `rivet-plugin`의 새 공개 API — 그리고 meet 한 번

```rust
/// 로더가 이 plugin의 권한에 대해 내린 판단을, guard가 필요한 모양으로.
///
/// `PermissionSet` 하나가 아니라 두 필드인 이유: "매니페스트가 요청하지 않았다"와
/// "프로파일이 깎았다"는 `effective` 안에서 **같은 부재**이고 운영자에게는 다른 문장이다.
/// 하나는 매니페스트를 고치라는 말이고 하나는 `--profile`을 바꾸라는 말이다.
/// 이름 붙인 타입인 것은 `plugin.md` §4.2가 예고한 세 번째 항(`∩ session overrides`)이
/// 왔을 때 시그니처를 또 깨지 않기 위해서다.
#[derive(Clone, Debug, Default)]
pub struct Grant {
    /// `manifest ∩ profile` — `PluginRecord::effective`.
    pub effective: PermissionSet,
    /// 매니페스트가 요청했는데 프로파일이 통째로 없앤 것 — `PluginRecord::denied`.
    pub denied: Vec<Permission>,
}

/// 이 구독자가 실제로 받을 토픽: 자기 선호를 grant와 meet한 것.
///
/// `None`은 "전부"이고, **빈 결과는 `None`이 아니라 에러다** -- 빈 목록은 배달 쪽에서
/// "전부"로 읽히므로 권한 0인 구독자가 전부를 받게 된다.
///
/// meet은 새로 쓰지 않는다. `Permission::EventsSubscribe`의 meet이 이미 접두사 반순서를
/// 안다(`tool.` ⊓ `tool.execute.` = `tool.execute.`, `tool.` ⊓ `run.` = 없음).
pub fn effective_topics(
    grant: &Grant,
    wanted: &[String],             // subscriber.topics(); 빈 것은 "전부"
) -> rivet_core::Result<Option<TopicScope>> {
    // grant 안의 `EventsSubscribe`는 하나라는 보장이 없다: `PermissionSet::new`의 dedup은
    // 값이 같을 때만 접고(`capability.rs:412`), `intersect`는 맞는 쌍마다 결과를 밀며
    // (`capability.rs:452`), 매니페스트 파서는 `[[permissions]]`를 그대로 나열한다. 첫
    // 번째를 고르면 조용히 좁히거나 넓힌다 -- plugin은 둘 다 가진 것이 맞으므로 join한다.
    // (join: 어느 하나가 `None`이면 `None`(전부), 아니면 접두사 합집합을 `TopicScope::new`에
    //  태운다 -- 그 생성자가 덮인 접두사를 흡수하므로 결과는 antichain이다.)
    let Some(granted) = join_subscribe_grants(grant.effective.granted()) else {
        return Err(/* grant.denied 가 EventsSubscribe 를 담고 있으면 "프로파일이 깎았다",
                      아니면 "매니페스트가 요청한 적 없다" -- 두 문장 (§5-4) */);
    };
    let wanted = Permission::EventsSubscribe(TopicScope::new(wanted.to_vec()).ok());
    match granted.meet(&wanted) {
        Some(Permission::EventsSubscribe(scope)) => Ok(scope),
        _ => Err(Error::plugin(/* 양쪽 목록을 다 적는다 */)),
    }
}
```

guard는 활성 프로파일의 **이름**을 모른다 — `PermissionSet`도 `PluginLoader`도 이름을
나르지 않는다(`plugin show`는 CLI에서 받는다). 그래서 "프로파일이 깎았다" 쪽 메시지는
이름을 대는 대신 `rivet plugin show <id>`를 가리킨다. 그 명령은 이름을 알고 이미
"removed by profile `readonly`"를 출력한다(`plugin_cmd.rs:203`). 이름을 넣으려면
`PluginLoader::new`에 인자를 하나 더해야 하고, 그건 이 문장 하나가 살 값이 아니다.

`TopicScope::new`가 빈 입력에 `Err`를 주므로 `.ok()`가 `None`이 되고, `meet`의
`(None, Some(list)) => Some(list)` 분기가 "빈 `topics()`는 grant의 목록"을 **공짜로**
만들어 준다. 겹침이 없으면 `meet`이 `None`을 주고 그게 우리의 `Err`다.

| grant | `topics()` | 배달 |
|---|---|---|
| `None`(전체) | 비었음 | 전부 |
| `None`(전체) | `["tool."]` | `tool.` |
| `Some(["tool.","plugin."])` | 비었음 | `tool.` + `plugin.` ← **오늘은 "전부"다** |
| `Some(["tool."])` | `["tool.execute."]` | `tool.execute.` (긴 쪽) |
| `Some(["tool."])` | `["job."]` | **등록 거부** |
| grant에 `EventsSubscribe` 없음 | 무엇이든 | **등록 거부** (`denied`가 두 문장을 가른다) |
| `Some(["tool."])` + `Some(["plugin."])` 둘 | 비었음 | `tool.` + `plugin.` (join, 첫 번째가 아니라) |

```rust
pub struct GuardedRegistry { /* + grant: Grant */ }
impl GuardedRegistry {
    pub fn new(inner: ScopedRegistry, plugin_id: PluginId,
               declared: Vec<CapabilityKind>, grant: Grant) -> Self;
}

/// 구독자를 감싸 `topics()`만 유효 목록으로 바꾼다. `name()`과 `on_event`는 위임한다 --
/// 레지스트리 키와 랙 보고의 이름이 plugin이 고른 것 그대로 남아야 한다.
#[derive(Debug)]
pub struct ScopedSubscriber { /* inner: Arc<dyn EventSubscriber>, topics: Vec<String> */ }
```

### 4.5 `rivet-tui`의 공개 API

```rust
/// 이벤트만으로 만들어지는 화면 상태. 런타임 타입이 하나도 없다.
#[derive(Debug, Default)]
pub struct AppState { /* run: RunView, jobs: JobView, status: StatusView, focus: Panel */ }

impl AppState {
    /// 순수 fold. 시계도, I/O도, 전역 상태도 읽지 않는다 -- `SessionState::replay`와 같은
    /// 성질이고, 패널을 손으로 만든 envelope 목록으로 테스트할 수 있게 하는 것이 이것이다.
    pub fn apply(&mut self, envelope: &EventEnvelope);
}

pub fn draw(frame: &mut ratatui::Frame<'_>, state: &AppState);

/// 사용자가 원하는 것. TUI는 런타임에 손을 뻗지 않고 이것을 밖으로 낸다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent { Quit, Cancel }

/// `EventSubscriber` 이면서 화면. 호스트가 `bus.observe(tui.clone())`으로 붙이고
/// `tui.render_loop(..)`를 따로 띄운다. 둘은 서로를 막지 않는다.
#[derive(Debug)]
pub struct Tui { /* state: Mutex<AppState>, intents: mpsc::Sender<Intent> */ }

impl Tui {
    /// 틱마다 그리고, 키를 `Intent`로 바꿔 내보낸다. 토큰이 취소되면 반환한다.
    ///
    /// 호스트가 취소에 쓰는 것과 **같은 타입**을 받는다(`run.rs:156`). `tokio-util`은
    /// 서드파티 crate이지 런타임 타입이 아니므로 DoD 1과 무관하다.
    pub async fn render_loop(&self, cancel: tokio_util::sync::CancellationToken)
        -> std::io::Result<()>;
}
```

패널이 보여주는 것과, 그것을 나르는 이벤트:

| 영역 | 내용 | 출처 |
|---|---|---|
| Agent | 스트리밍 텍스트 · turn 번호 · 재시도 · tool 호출 줄(이름/상태/ms) · 정지 사유 | `agent.*`, `tool.*` |
| Job | Job별 상태 체크리스트, run 부착, 리뷰 verdict | `job.*` — **Phase 3에는 생산자가 없다.** 비어 있을 때 "no jobs — the job runtime lands in Phase 5"를 그린다 |
| 상태바 | session · run · model · turn · 토큰(in/out) · tool 수 · **dropped** · 경과 | envelope의 상관관계 필드 + `agent.run.started` + `agent.request.completed` + `runtime.subscriber.lagged` |

상태바의 `dropped`가 `runtime.subscriber.lagged`에서 온다는 것이 이 설계의 자기 일관성
지점이다 — 버스가 유실을 보고하기 때문에 UI가 유실을 보여줄 수 있다. 정직한 단서 하나:
랙에 걸린 구독자는 **자기 랙 보고도 놓칠 수 있다.** 상태바의 숫자는 "지금까지 본 보고의
합"이지 "잃은 것의 총계"가 아니다(§7-6).

### 4.6 CLI 표면과 스트림

```text
$ rivet --tui "explain this repo"          # --jsonl · --headless 와 상호 배타
$ rivet --tui < /dev/null | cat            # 파이프면 exit 2: "`--tui` needs a terminal"

$ rivet --jsonl "read the readme" | jq -r .payload.event
runtime          # runtime.started  ← 오늘은 없다: 관측자가 plugin 로드 뒤에 붙는다
plugin           # plugin.discovered, 카탈로그의 모든 id에 대해 한 줄씩
                 #   -- telemetry-log 도 여기 나온다. discover는 선택과 무관하다.
plugin           # plugin.loaded, 선택된 것에 대해서만 (§3.6 default_selection)
agent            # agent.run.started …
tool
agent
runtime          # runtime.shutting_down
plugin           # plugin.unloaded, 로드됐던 것에 대해 한 줄씩
```

`runtime.shutting_down`이 `plugin.unloaded`보다 **먼저** 나온다. 순서가 그게 맞다 — 종료를
알린 다음 정리한다. 그리고 그 순서 때문에 unload되는 plugin의 구독 태스크는
`runtime.shutting_down`까지는 보고 그 뒤 `plugin.unloaded`는 못 본다. 그게 DoD 5가 요구하는
것이다.

### 4.7 토픽 28개, 어디서 나오는가 (3.1의 산출물)

| 패밀리 | 토픽 | 발행 지점 |
|---|---|---|
| agent | 8개 전부 | `agent_loop.rs` (이미) |
| tool | `requested` `execute.started` `execute.progress` `execute.completed` `blocked` | `dispatch.rs` (이미) |
| tool | `policy.evaluated` `approval.requested` `approval.resolved` | **Phase 4** — `dispatch.rs`의 "4 Intercept, 5 Policy, 6 Approval" 주석 자리 |
| job | 5개 전부 | **Phase 5** |
| plugin | 4개 전부 | `loader.rs` (이미) |
| runtime | `subscriber.lagged` | `bus.rs::pump` (이미; Phase 3이 `observe` 경로도 같은 펌프로 덮는다) |
| runtime | `started` `shutting_down` | **Phase 3 신규** — `lifecycle.rs`, 호출자는 `run.rs` |

18 → 20. 나머지 8은 Phase 4(3) · Phase 5(5)이고, 그 분류가 코드가 아니라 **테스트가
붙드는 목록**이 된다는 것이 3.1의 실질이다.

---

## 5. 실패 모드

| # | 실패 | 처리 |
|---|---|---|
| 1 | 느린 구독자가 루프를 붙든다 | 구조적으로 불가능하다. `publish`는 동기·무오류이고 배달은 별도 태스크다. **측정**으로 확인한다 (DoD 2). |
| 2 | 구독자가 뒤처져 이벤트를 잃는다 | 잃고 **보고한다**. `pump`가 `RecvError::Lagged(n)`을 받으면 `runtime.subscriber.lagged { subscriber, dropped: n }`을 발행한다. `attach`와 `observe`가 같은 펌프를 쓰므로 호스트 관측자도 보고된다. |
| 3 | 랙 보고 자체가 유실을 늘린다 | 보고는 `Lagged` 결과 하나당 하나이고 `Lagged`는 오버플로 구간당 한 번이므로, 보고 수의 상한은 이벤트 수가 아니라 **유실 구간 수**다. 되먹임 폭주는 없다. 다만 랙에 걸린 구독자가 자기 보고를 놓칠 수는 있다 (§4.5). |
| 4 | plugin이 `events_subscribe` 없이 구독자를 등록한다 | `Err`. 두 경우를 메시지가 구분한다 — 매니페스트가 요청하지 않았거나, 프로파일이 깎았거나. `effective` 안에서는 둘 다 똑같은 부재이므로 **가르는 것은 `grant.denied`다**: 거기 `EventsSubscribe`가 있으면 프로파일이 깎은 것이다(`loader.rs:194-201`이 채운다). 운영자가 할 일이 다르므로 문장이 달라야 한다 — 매니페스트를 고치거나, `--profile`을 바꾸거나. 후자는 `rivet plugin show <id>`를 가리킨다(guard는 프로파일 이름을 모른다, §4.4). |
| 5 | `topics()`가 grant와 겹치지 않는다 | `Err`, 양쪽 목록을 다 적는다. 조용히 빈 목록으로 넘기면 "전부"가 되므로 정반대로 실패한다(§2.1-c). |
| 6 | 매니페스트에 `event_subscriber` 슬롯이 없다 | 기존 guard가 그대로 거부한다. 이 검사는 §5-4와 **다르다**: 슬롯은 "무엇을 채우는가", permission은 "무엇을 볼 수 있는가"이고, 메시지도 둘로 남는다. |
| 7 | `readonly`에서 telemetry plugin이 대화 전문을 요구한다 | **guard는 이걸 못 잡는다.** `meet_prefixes`는 합집합이라 기본 여섯 개가 살아남고 `agent.text.`만 사라지므로 meet은 비지 않고 §5-5는 발동하지 않는다. 잡는 것은 plugin 자신이다: 운영자가 *적어 넣은* `topics`나 `include_conversation = true`가 meet에서 사라지면 `load`가 `Err`로 실패한다(관용구 (1), §4.2의 표). 아무것도 안 적은 기본 목록은 좁혀진 채로 로드된다 — 그쪽은 *선호*다. |
| 7b | 매니페스트가 `events_subscribe`를 두 줄 적는다 | `effective`에 `EventsSubscribe`가 둘 남는다(`PermissionSet::new`의 dedup은 값이 같을 때만 접는다). 첫 번째를 고르면 조용히 좁히거나 넓히므로 **join**한다 — 하나라도 `None`이면 전부, 아니면 접두사 합집합을 `TopicScope::new`에 태워 흡수시킨다(§4.4). |
| 8 | 등록된 구독자가 이벤트를 못 받는다 | Phase 0의 "등록이 곧 attach"가 이미 막았다. Phase 3은 그것을 실제 plugin으로 증명한다 (DoD 4). |
| 9 | 언로드된 plugin이 계속 관찰한다 | `unregister_all`이 소유자별 `JoinHandle`을 abort한다. Phase 3은 로더를 지나는 경로로 증명하고, 구독자 `Arc`가 **떨어지는 것**까지 본다 — 배달이 멈춘 것과 태스크가 끝난 것은 다른 주장이다 (DoD 5). |
| 10 | `on_event`가 양보하지 않는다 | abort가 안 먹는다. 아래 "알려진 미해결"을 보라. |
| 11 | 스트림 꼬리가 잘린다 | `drain_within(2 s)`. 초과하면 `Drained::Truncated`를 stderr에 한 줄로 알린다 — 잘렸다는 것을 소비자가 아는 편이 낫다. |
| 12 | `--tui`가 파이프에서 실행된다 | 시작 전에 `Err`, exit 2. raw mode를 파이프에 걸면 복구할 터미널이 없다. |
| 13 | 긴 실행이 TUI 메모리를 먹는다 | 텍스트·tool 목록·job 목록은 상한 있는 링 버퍼다. 넘치면 패널 머리에 `… N줄 생략`. |
| 14 | 패닉이 터미널을 raw mode로 남긴다 | `TerminalGuard`의 `Drop` + 패닉 훅. 여기서는 await가 필요 없으므로 `Drop`이 맞는 도구다 (`Sandbox`가 `Drop`을 못 쓰는 이유와 대비). |
| 15 | raw mode가 Ctrl-C를 삼킨다 | **Phase 1의 보장을 조용히 깨는 자리다.** raw mode에서 SIGINT는 키 이벤트로 온다. TUI가 `Ctrl-C`를 `Intent::Cancel`로 매핑하고 호스트가 signal handler와 **같은 토큰**을 취소한다. 테스트로 못 박는다. |
| 16 | telemetry 로그가 stderr을 막는다 | `tracing` sink 쓰기는 동기다. 파이프가 막히면 그 펌프가 선다. 루프는 안 선다(§5-1) — 서는 것은 그 구독자 하나이고, 대가는 §5-10이다. |
| 17 | `--jsonl`과 telemetry plugin이 같이 켜진다 | 목적지가 다르다 — 전자는 stdout, 후자는 `tracing` sink(기본 stderr)이므로 스트림이 섞이지는 않는다. 다만 **human 렌더러와는 stderr을 공유한다** — `→ read_file` 줄 사이에 로그 줄이 낀다. 이건 opt-in 조합이고, 답은 `RIVET_LOG_FORMAT=json rivet ... 2>telemetry.jsonl`이다. `config.md`에 그렇게 적는다. |
| 17b | **`--tui`와 telemetry plugin이 같이 켜진다** | 이 표가 놓쳤던 조합이다. 17의 답(리다이렉션)이 여기서는 안 통한다 — TUI가 소유한 것이 그 터미널이고, ratatui는 자기 버퍼에 대해 diff하므로 프레임 위에 찍힌 줄은 다시 그려지지 않는다. 기본 필터가 `rivet_telemetry_log=info`이므로 이벤트당 한 줄이고, plugin이 없어도 아무 `warn!` 하나면 같다. `--tui`이면서 stderr이 그 터미널이면 sink를 버린다. `2>run.log`로 돌린 stderr은 부딪힐 것이 없으므로 그대로 둔다 — 그래서 판단 기준이 stdout을 보는 `is_a_terminal`이 아니라 stderr이다. |
| 18 | 이벤트 패밀리가 새로 생겨 좁힌 프로파일이 조용히 못 받는다 | `Event::one_of_each()` 위의 **와일드카드 없는 두 층 `match`**가 컴파일 타임에 잡는다 — 패밀리는 바깥 `match`, 변형은 안쪽 `match`. `&str` 키로 일반화하면 오늘 있는 컴파일 성질이 런타임으로 내려앉는다(§3.1). §11-10이 "silent"라고 적어 둔 구멍이다. |
| 19 | 관측 사이드카가 설정 없이 켜진다 | `PluginSelection::All`은 카탈로그 전부이므로(`catalog.rs:145`) 카탈로그에 넣는 것만으로 기본 켜짐이 된다. `catalog::default_selection()`이 `All`의 뜻을 "에이전트를 돌리는 데 필요한 것"으로 좁힌다(§3.6). discover·`plugin list`·`enabled`로 이름 대기는 전부 그대로다. |

### 알려진 미해결 — `Plugin::load`/`unload`와 그것들이 띄운 태스크에 타임아웃이 없다

[`architecture.md` §11-15](../architecture.md)가 명명한 위험이고, **이번에 고치지 않는다.**
다만 그 판단을 네 줄로 남긴다.

**Phase 3이 위험을 늘리지 않는다.** `telemetry.log`의 `load`는 설정을 읽고 구독자 하나를
등록하고 끝난다 — 네트워크도, 파일도, 기다림도 없다. `unload`는 no-op이다(펌프는
레지스트리가 abort한다). §11-15가 그리는 형태, 즉 "닿지 않는 백엔드를 `load` 안에서
기다리는" 모양이 이 plugin에는 없다. 표면은 하나 늘고 위험은 안 는다.

**그러나 Phase 3에만 있는 변종이 하나 생긴다.** `JoinHandle::abort`는 다음 await 지점에서
효력이 있는데, `on_event` 안의 동기 블로킹(막힌 파이프로의 `tracing` 쓰기 같은)에는 그
지점이 없다. 그래서 **DoD 5는 정확히는 "협조적인 구독자에 대해" 참이다.** 경계는 좁다:
루프는 멈추지 않고(그게 DoD 2다), 멈추는 것은 그 구독자의 펌프 하나이며, 레지스트리 표에서
이미 사라졌으므로 `unload`는 반환하고 이름은 재로드에 쓸 수 있다. 회계는 어긋나지 않고
태스크 하나가 프로세스 끝까지 샌다. 이 문장을 `plugin.md` §4.1에 적는다.

**그래도 예산을 발명하지 않는 이유는 Phase 2와 같다.** §11-15가 열어 둔 것은 숫자가 아니라
**초과했을 때 무엇을 남기는가**(`FAILED` 레코드인가, 호스트 중단인가)이고, 그건 프로세스
경계가 "닿지 않는 상대"를 예외가 아니라 일상으로 만드는 Phase 6의 결정이다. 설계가 지정하지
않은 예산은 그 자체로 새 실패 모드다 — 부하 걸린 CI에서 느린 `load`가 `FAILED`가 된다.

**대신 Phase 3이 공짜로 주는 것: 이 구멍이 처음으로 *보인다*.** 지금은 매달린 `load`가
아무 신호도 안 남긴다 — `rivet run`이 시작 지점에서 메시지도 `FAILED` 레코드도 없이 선다.
Phase 3 뒤에는 `runtime.started`가 plugin 로드 **전에** 발행되고 관측자가 그 전에 붙으므로,
매달린 `load`는 "`runtime.started`와 그 id의 `plugin.discovered`는 있는데 `plugin.loaded`도
`plugin.load.failed`도 없는 스트림"으로 드러난다. 진단이 생겼지 데드라인이 생긴 것은
아니고, 문서에도 그렇게 적는다. 테스트: `a_load_that_hangs_shows_up_as_a_discovered_plugin_that_never_loaded`.

---

## 6. 테스트 전략

전부 offline (`cargo test --workspace --offline`). 유지하거나 올려야 할 기준선:
**456 passed · clippy 0 · `cargo doc` 0**. 사라지는 테스트는 없다.

### 6.1 DoD 6줄 — 각각 무엇이 증명하는가

| DoD (plan.md 원문) | 증명하는 테스트 |
|---|---|
| TUI가 런타임 타입을 **하나도** import 하지 않고 이벤트만 소비 | `the_tui_crate_does_not_depend_on_the_runtime` (`rivet-tui`) — 자기 `Cargo.toml`을 `toml`로 **파싱해** `[dependencies]`·`[dev-dependencies]` 어느 쪽에도 `rivet-runtime`이 없고 rivet 의존이 `rivet-core` 하나임을 단언한다(문자열 검색이 아닌 이유는 §3.5). 의존이 없으면 `use rivet_runtime::…`은 **컴파일되지 않으므로**, 이 테스트가 지키는 것은 성질이 아니라 그 성질을 되돌리는 편집이다. 그리고 양의 증명: `every_panel_is_filled_from_events_alone` (`rivet-tui`) — 다섯 패밀리의 envelope를 손으로 만들어 fold하고 Job 패널·Agent 패널·상태바를 전부 단언한다. 런타임도, 버스도, 터미널도 없이. |
| 느린 구독자가 루프를 지연시키지 않음 (측정) | `a_slow_subscriber_does_not_delay_the_loop` (`rivet-runtime/tests/event_flow.rs`) — 버스 용량 16, `on_event`가 이벤트당 20 ms 자는 구독자, 델타 400개를 흘리는 `FixtureModel`로 **실제 `AgentLoop`를 돌린다**. 직렬 배달이었다면 ≥ 8 s인데 run이 1 s 안에 끝남을 단언한다(여유 8배; 상한은 루프에만 걸고 구독자에는 안 건다). 같은 테스트가 랙 보고가 났음을 함께 단언한다. |
| `SubscriberLagged`가 실제로 발행됨 | `a_lagging_subscriber_is_reported_by_name_on_the_bus` (`event_flow.rs`) — 용량 8, oneshot에 막혀 있는 구독자 하나 + 빠른 기록자 하나. 이벤트 40개를 발행하고, 기록자가 `RuntimeEvent::SubscriberLagged { subscriber: "wedged", dropped: n }`을 `n > 0`으로 받았음을 단언한다. 막힌 구독자가 첫 이벤트에서 멈추므로 랙은 **결정적**이다. 기존 `a_slow_subscriber_lags_instead_of_stalling_the_publisher`는 수신자의 `RecvError`만 봤지 발행된 이벤트를 보지 않았다. |
| 등록된 subscriber plugin이 실제로 이벤트를 받음 (`attach_subscriber`) | `a_registered_subscriber_plugin_receives_events` (`rivet-plugin/tests/subscriber.rs`) — 진짜 `PluginLoader`로 `event_subscriber` + `events_subscribe`를 선언한 plugin을 로드하고, 버스에 발행하고, plugin의 sink가 봤음을 단언한다. 짝: `the_telemetry_plugin_logs_what_it_receives` (`plugins/telemetry-log`) — `tracing` 테스트 레이어로 필드까지 본다. |
| 언로드된 plugin의 구독 태스크가 중단됨 | `an_unloaded_plugins_subscription_task_stops` (`tests/subscriber.rs`) — 로더의 `unload`를 지나는 경로로, unload 전 관측 / unload 후 무관측. 그리고 `an_unloaded_subscriber_is_dropped_not_merely_silenced` — 구독자에 `Drop` 플래그를 달아 펌프가 들고 있던 `Arc`가 실제로 떨어졌음을 본다. "배달이 멈췄다"와 "태스크가 끝났다"는 다른 주장이고, DoD가 말하는 것은 후자다. |
| `--jsonl`이 관찰 가능성을 제공 — **세션 재구성용이 아님** | 두 줄이므로 두 테스트다. `jsonl_carries_the_whole_lifecycle_not_just_the_answer` (`rivet-cli/tests/e2e.rs`) — 한 번의 실행에서 `runtime.started` · `plugin.discovered` · `plugin.loaded` · `agent.run.started` · `tool.execute.started` · `tool.execute.completed` · `agent.run.completed` · `runtime.shutting_down` · `plugin.unloaded`가 전부 스트림에 있고 `runtime.started`가 **첫 줄**임을 단언한다. 그리고 `the_jsonl_stream_is_not_a_session_export` — 같은 세션에 대해 `--jsonl` 출력에는 `user.message`·`assistant.message`·`seq`가 **하나도 없고** `rivet session show --json`에는 셋 다 있음을 단언한다. 이 대비가 plan.md 주석이 말하는 바로 그것이다: durable fact는 버스에 없다. |

### 6.2 작업 항목별

**3.1 — 발행 지점.** `every_bus_topic_is_claimed` (`event_flow.rs`): `Event::one_of_each()`를
**와일드카드 없는 두 층 `match`**로 훑어 각 변형을 `PublishedInPhase3` 또는
`Deferred(phase)`로 분류하고, 그 결과가 두 기대 목록과 정확히 같음을 단언한다. 변형이
새로 생기면 `match`가 **컴파일에 실패하고**(arm 없음), arm을 더하면 기대 목록에 없어서
실행에 실패한다. 패밀리가 새로 생겨도 바깥 `match`가 같은 순서로 잡는다. 이 두 층이
§3.1이 말한 "one_of_each를 소비하는 두 `match`" 중 하나다 — 다른 하나는 `rivet-cli`의
프로파일 테스트다.
`a_run_publishes_every_agent_and_tool_topic_this_phase_owns`: 텍스트 스트리밍 + 진행 보고를
하는 tool + agent scope 밖 호출(→ `tool.blocked`) + 일시 실패(→ `agent.request.failed`)를
한 대본에 넣어 13개를 한 번에 관측한다.
`a_host_lifecycle_publishes_every_runtime_and_plugin_topic_this_phase_owns`: 시작 → discover →
load(하나는 실패시켜 `plugin.load.failed`) → unload → shutting_down.
`runtime_started_precedes_everything_it_would_describe`: `runtime.started`가
`plugin.discovered`보다 먼저임을 순서로 단언한다.
core 쪽: `every_sample_has_a_distinct_topic` — `Event::one_of_each()`가 내는 토픽이 전부
서로 다름(복붙 중복 감지). `one_of_each`는 `vec![]` 리터럴이라 **변형 누락 자체를 잡지
못하고**, 그것을 잡는 것은 위의 두 `match`다. 여기서 그 이상을 주장하지 않는다.

**3.2 — 등록 + 토픽 필터.** (`rivet-plugin/tests/subscriber.rs`)
`a_plugin_without_events_subscribe_cannot_register_a_subscriber` ·
`a_manifest_that_never_asked_and_a_profile_that_removed_it_say_different_things` ·
`an_empty_topics_list_becomes_the_grant_not_everything` ·
`a_subscriber_narrower_than_its_grant_keeps_its_own_filter` ·
`a_grant_narrower_than_the_subscriber_wins_with_the_longer_prefix` ·
`a_subscriber_whose_topics_fall_outside_its_grant_is_refused` ·
`a_readonly_profile_keeps_the_conversation_from_a_subscriber` — 모든 토픽을 원하는
구독자를 `readonly` grant로 로드하고, `tool.*`는 받고 `agent.text.delta`는 **한 번도** 안
받음을 단언한다. §11-10을 닫는 테스트이므로 그 항목이 이 이름을 인용한다.
`a_subscribers_own_name_survives_the_wrapper` — 레지스트리 키와 랙 보고의 이름이 plugin의
것임을 본다. `two_subscribe_grants_join_rather_than_the_first_winning` (§5-7b) — 매니페스트가
`events_subscribe`를 두 줄 적었을 때 배달이 두 scope의 합집합임을 단언한다.
`rivet-cli` 쪽: `every_topic_is_granted_or_deliberately_withheld` (기존 agent 한정 테스트를
`Event::one_of_each()` 위의 두 층 `match`로 일반화 — `&str`이 아니라 값으로 훑는 이유는
§3.1).

`a_manifest_that_never_asked_and_a_profile_that_removed_it_say_different_things`는 이번
설계에서 **비로소 성립한다**: guard가 `effective`만 받으면 두 경우가 같은 부재라 구분할
값이 없었다(리뷰 1 blocking). `Grant`가 `denied`를 함께 나르므로 이제 두 문장을 만들 수
있고, 테스트는 같은 fake plugin을 ① `events_subscribe` 없는 매니페스트 ② 있는 매니페스트 +
그것을 안 주는 grant, 두 번 로드해 두 에러 메시지가 서로 다르고 각각 옳은 조치를
가리킴을 단언한다.

**3.3 — `telemetry.log`.** `the_manifest_matches_the_crate` (카탈로그 규약) ·
`the_default_topics_leave_out_the_conversation` ·
`the_default_topics_survive_every_shipped_profile` (다섯 프로파일 전부에서 meet이 기본
목록을 그대로 남김 — 설정 없는 telemetry는 어디서든 뜬다) ·
`include_conversation_off_means_it_does_not_even_subscribe` ·
`configured_topics_replace_the_default` ·
`configured_topics_the_profile_narrows_are_refused_not_silently_dropped` ·
`a_readonly_profile_refuses_a_telemetry_plugin_that_was_told_to_log_the_conversation`
(이름을 고쳤다: guard의 빈-meet 거부가 아니라 **plugin 자신의** 관용구 (1)이 하는 일이다,
§4.2·§5-7) ·
`every_log_record_names_its_topic_and_its_run` ·
e2e: `the_telemetry_plugin_logs_a_run_when_enabled` (`enabled`에 명시적으로 넣고
`RIVET_LOG_FORMAT=json`으로 돌려 stderr 줄이 파싱되고 `topic` 필드를 가짐) ·
`the_telemetry_plugin_is_not_in_the_default_selection` (설정 파일 없는 실행에서 discover는
됐고 로드는 안 됐음을 `rivet doctor` 출력으로 단언 — 리뷰 1 blocking).

**3.4 — `--jsonl`.** DoD 6의 둘에 더해 `a_truncated_drain_says_so_on_stderr`와
기존 `jsonl_output_is_one_parseable_event_per_line`(변경 없이 통과해야 한다 — 회귀 감시).
카탈로그 쪽: `an_opt_in_plugin_is_absent_by_default_and_loads_when_named` ·
이름을 고친 `with_no_config_file_every_plugin_the_default_selection_names_is_loaded`.

**3.5 — TUI.** `the_tui_crate_does_not_depend_on_the_runtime`는 `toml`로 자기 매니페스트를
**파싱**한다 — 문자열 검색이면 주석에 오탐하고 `rivet-runtime.workspace = true`라는 표기를
놓칠 수 있다. 나머지: `the_agent_panel_follows_a_run_from_start_to_stop` ·
`the_job_panel_renders_what_job_events_carry` (job 런타임 없이 `job.*` envelope만으로) ·
`the_job_panel_says_where_jobs_come_from_when_empty` ·
`the_status_bar_counts_drops_from_the_lag_report` ·
`the_status_bar_shows_only_what_events_carry` (§7-2를 못 박는 음의 단언) ·
`quitting_asks_the_host_rather_than_reaching_for_the_token` (`Intent`가 나오고 상태는 안
바뀜) · `ctrl_c_in_the_tui_cancels_the_run` (§5-15) ·
`the_terminal_is_restored_when_the_ui_stops` · `draw` 세 패널을 ratatui `TestBackend`로
스냅샷하는 `the_three_panels_fit_an_eighty_column_terminal` ·
CLI: `tui_refuses_a_pipe`.

### 6.3 게이트

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # 새 공개 타입은 전부 Debug 필요
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

`missing_debug_implementations`가 `warn` + `-D warnings`이므로 `Observer`·`Drained`·
`ScopedSubscriber`·`AppState`·`Tui`·`Intent`·`TerminalGuard`는 전부 `Debug`를 가진다.
`Tui`는 `Mutex<AppState>`와 `mpsc::Sender`를 들므로 수동 구현이 필요하다.
타이밍에 의존하는 테스트는 DoD 2 하나뿐이고, 그 여유(8배)와 "상한은 루프에만 건다"는
선택을 테스트 안 주석에 적는다.

---

## 7. 열린 질문

과제 서술이 정하지 않은 것들. 각각 이 설계가 쓰는 잠정 답을 **[가정]**으로 달아 두어 빌드가
막히지 않게 하되, 가정임을 숨기지 않는다.

1. **`--tui`가 opt-in인가 기본인가.** plan.md는 3.5를 "기본 TUI"라고만 부르고 언제 뜨는지는
   말하지 않는다. **[가정]** Phase 3에서는 `--tui` opt-in. 기본을 뒤집는 것은 승인
   프롬프트가 TUI의 소유가 되는 Phase 4.4에서 하는 편이 자연스럽고, 지금 뒤집으면 Phase 1
   e2e의 stdout/stderr 단언을 전부 다시 써야 한다.
2. **상태바가 프로파일과 워크스페이스 루트를 못 보여준다.** 어떤 이벤트도 나르지 않는다
   (`RuntimeEvent::Started`는 `version`뿐). **[가정]** 안 넣는다. 넣으려면 `rivet-core`
   변경이고, "TUI는 이벤트가 나르는 것만 본다"는 규칙이 처음으로 압력을 받는 자리이므로
   설계가 임의로 정할 것이 아니다. 필요하다고 판단되면 `Started`에 필드를 더하는 것이
   import보다 낫다는 것만 적어 둔다.
3. **`events_publish`가 강제되지 않는다.** `ctx.events`는 grant와 무관하게 통째로
   넘어가므로 plugin이 위조 `agent.*`를 발행할 수 있다. 모든 프로파일이 이 권한을 주므로
   오늘은 아무것도 안 터진다. **[가정]** 그대로 둔다. 한 줄로 막을 수는 있지만(권한이
   없으면 널 버스를 넘긴다) 조용히 아무 일도 안 하는 버스는 에러보다 나쁘고, 진짜 답은
   발행자를 envelope에 스탬프하는 것이며 그건 프로세스 경계가 생기는 Phase 6의 모양이다.
   §11에 올릴 후보다.
4. **`runtime.started`를 누가 발행하는가.** `doctor`와 `plugin list`도 버스를 만든다.
   **[가정]** `rivet run`/`rivet resume`만 발행한다. `doctor`는 진단이지 런타임 시작이
   아니고, 진단이 `runtime.started`를 흘리면 그 토픽의 뜻이 흐려진다.
5. **`CapabilityKind::Command`.** [`plugin.md` §1](../plugin.md) 표가 "CLI 하위 명령
   (Phase 3)"이라고 적고 있는데 plan.md의 3.1–3.5에는 없다. **[가정]** 범위 밖 —
   작업 항목의 출처는 plan.md다. 문서 한쪽이 틀렸고, 어느 쪽인지는 이 설계가 정하지
   않는다. `plugin.md`에 "(Phase 3에는 없다)" 각주만 단다.
6. **랙 보고의 `dropped`를 상태바가 어떻게 세는가.** tokio가 주는 것은 recv 사이의 드롭
   수다. **[가정]** 누적 합. 다만 그 합은 "본 보고의 합"이므로 하한이며, 상태바 도움말에
   그렇게 적는다.
7. **자기 랙 보고를 자기가 놓칠 수 있다.** 별도 채널로 빼면 확실해지지만 그건 lossy 버스
   설계를 깨는 예외 경로다. **[가정]** 받아들이고 §4.5에 적는다.
8. **`telemetry.log`가 기본으로 켜지는가.** ~~[가정]~~ **결정됨 (리뷰 1 blocking).** 안
   켜진다. 그리고 그것을 `rivet.example.toml`의 주석으로 맡길 수 없다 — `PluginSelection::All`
   (= `enabled` 키가 없거나 빈 것)이 카탈로그 전부로 풀리므로(`catalog.rs:145`,
   `config.rs:542-545`) 카탈로그에 한 줄 넣는 것만으로 **설정 파일 없는 트리에서 로드된다.**
   `catalog::default_selection()`이 `All`의 뜻을 좁힌다(§3.6). 남은 판단은 이 개념이 벌어
   먹는가인데, plan.md의 MVP 경계 표가 MVP+에 `telemetry(otel · prometheus)`를 두고 있으므로
   같은 모양이 최소 둘 더 온다.
9. **drain 예산 2초는 어디서 왔나.** 아무 데서도 안 왔다. **[가정]** 2 s, 초과 시 stderr
   한 줄. §11-15와 달리 이건 **호스트가 자기 출력에 거는 예산**이라 plugin 계약이 아니고,
   틀렸을 때의 대가는 "꼬리가 잘렸다는 경고"뿐이다. 그래서 발명해도 된다고 판단했다 —
   그 구분이 §11-15를 열어 두는 이유이기도 하다.
10. **기본 EnvFilter를 `warn`에서 `warn,rivet_telemetry_log=info`로 바꾸는 것.**
    plugin이 켜져 있는데 아무것도 안 보이면 3.3이 "대충 됨"이다. **[가정]** 바꾼다.
    `RUST_LOG`가 있으면 여전히 통째로 이긴다. §7-8이 닫히면서 이 변경의 위험은 사라졌다 —
    plugin이 기본 선택에 없으므로 아무도 안 켠 실행에서는 이 지시어가 가리킬 대상이 없고,
    기본 경로의 stderr는 오늘과 같다. 켠 경우 human 렌더러와 stderr을 공유하는 것은
    §5-17이 다룬다. CLI의 기본 동작 변경인 것은 여전하므로 `config.md`에 적는다.
11. **`GuardedRegistry::new`가 공개 생성자인데 인자가 하나 는다.** 모듈 doc이 "embedder가
    자기 것을 만들 수 있다"고 적는다. **[가정]** 깨는 변경을 그냥 한다 — 0.1이고 Phase 2가
    막 실은 API다. 중요한 것은 방향이다: `Grant::default()`는 `effective`도 `denied`도
    비어 있으므로 모든 구독자가 "매니페스트가 요청한 적 없음"으로 **거부**된다. 잊은
    embedder는 조용히 허용되는 게 아니라 닫힌다.

---

## 8. 구현이 설계에서 벗어난 곳

설계가 **틀렸던** 것 하나, 리뷰 2라운드가 non-blocking으로 남긴 것을 구현이 정한 것 여덟.
나머지 §3 항목은 쓰인 대로 만들어졌다.

---

### 1. 랙 보고가 자기 자신을 먹여 살린다 — §5-3이 틀렸다

**설계** — §5-3: "보고는 `Lagged` 결과 하나당 하나이고 `Lagged`는 오버플로 구간당 한
번이므로, 보고 수의 상한은 이벤트 수가 아니라 **유실 구간 수**다. 되먹임 폭주는 없다."

**부딪힌 것** — 되먹임 폭주가 있다. 보고 자체가 `publish`이고, 꽉 찬 채널로의 `publish`는
가장 오래된 슬롯을 덮어쓴다. 그런데 `Lagged` 직후 tokio가 그 수신자를 재배치하는 자리가
정확히 그 슬롯이다. 그래서 `Lagged` 하나당 보고 하나는 **자기가 다음 `Lagged`를 만든다**:
보고 → 1만큼 랙 → 보고 → 1만큼 랙 → …, 이벤트를 하나도 배달하지 않은 채로.

측정: 용량 8 버스에 뒤처진 구독자 둘. **발행 40개가 118,312개가 될 때까지** 어느 구독자도
이벤트를 하나도 못 받았다. DoD 2와 DoD 3의 랙 단언이 둘 다 이것 때문에 처음에 실패했다 —
기록자가 랙 보고를 받기는커녕 아무것도 못 받았다.

**한 것** — `pump`이 `Gap`을 들고 **구간당 한 번만** 보고한다. 구간은 무언가 실제로
배달됐을 때 닫힌다. 보고 뒤 배달 전에 세어진 드롭은 보고되지 않고, 그게 맞다 — 그것들은
그 보고가 만든 드롭이다. 회귀 테스트: `a_lag_report_does_not_feed_itself_into_a_runaway`.

**Phase 3이 만든 버그가 아니다.** `attach`가 Phase 0부터 같은 모양이었고, 얕은 버스에
뒤처진 구독자를 둘 올린 적이 없어서 안 드러났을 뿐이다. `events.md` §5가 이제 이 성질을
적는다.

### 2. §4.4의 `GuardedRegistry::new` 시그니처 — 문서의 나머지를 따랐다

리뷰 2 non-blocking. §4.4의 코드 블록만 리뷰 1 blocking 2의 수정이 안 들어가
`effective: PermissionSet`으로 남아 있었고, 같은 절의 `Grant` 정의 ·
`effective_topics(grant: &Grant, ..)` · §3.3 · §5-4 · §6.2 · §7-11은 전부 `Grant`를
전제한다. 문서의 나머지를 정본으로 보고 구현했으며, 위 §4.4의 블록도 그렇게 고쳤다.

### 3. `an_absent_enabled_list_selects_the_whole_catalog` — 이름을 고쳤다

리뷰 2 non-blocking. `default_selection()`이 들어가면 이 단위 테스트의
`assert_eq!(chosen.len(), sources().len())`이 4 ≠ 3으로 깨진다. 형제격 e2e는 설계가 이름까지
정해 고쳤는데 이 단위 테스트는 §6 목록에 없었다.

`an_absent_enabled_list_selects_the_default_not_the_whole_catalog`으로 고치고, 본문을
"기본 선택과 같은지" + "카탈로그보다 작은지" 둘 다 단언하도록 바꿨다. 사라지는 테스트는 없다.

### 4. `default_selection()`이 `sources()`와 어긋나지 않게 지키는 테스트를 더했다

리뷰 2 non-blocking. `select`의 `All` 분기는 검증을 안 하므로 `default_selection()`의 오타
id는 `load_selected`가 매번 실패하는 **모든 실행**의 죽음이 된다. 반대 방향만 잡는
`an_opt_in_plugin_is_absent_by_default_and_loads_when_named` 옆에
`every_default_selection_id_is_in_the_catalog`를 뒀다.

### 5. `doctor.rs`가 바뀌는 파일 목록에 들어간다

리뷰 2 non-blocking. §3.6의 rivet-cli 파일 목록에 `doctor.rs`가 빠져 있었다.
`catalog::load(config, bus)`로 바뀌었으므로 호출부도 바뀐다 — doctor는 버스를 스스로 만들어
넘기기만 하고 `lifecycle::started`는 부르지 않는다(§7-4).

### 6. guard가 좁힘을 관측했을 때 `tracing::debug!` 한 줄을 남긴다

리뷰 2 non-blocking(부분 겹침). §4.4의 배달 표는 "완전히 어긋남 → 등록 거부"만 다루고,
부분 겹침은 guard가 통과시키면서 아무 흔적도 안 남겼다.

**정한 것**: 거부하지 않는다. 운영자가 적은 것을 지키는 규칙은 plugin 몫이고(§4.2의 관용구),
호스트가 그것을 모든 구독자에게 강제하면 "기본 목록은 선호다"가 무너진다. 대신 좁혀졌다는
사실과 **사라진 접두사**를 `tracing::debug!`로 남긴다. 목록이 아니라 *커버리지*로 비교한다 —
`TopicScope`가 정렬·흡수하므로 같은 집합이 다른 순서로 돌아오고, 순서로 비교하면 매번
거짓 양성이 난다(구현 중 실제로 났다).

### 7. `effective_topics`의 `.ok()` — 빈 목록만 `None`으로 접는다

리뷰 2 non-blocking. `TopicScope::new(wanted).ok()`는 "빈 목록"과 "잘못된 목록"을 같은
`None`으로 접어, `topics() = ["tool.", ""]`을 돌려주는 구독자가 자기가 적은 것보다 **넓은**
grant 전체를 받게 한다. 빈 입력만 `None`으로 접고 비어 있지 않은 입력의 `Err`는 그대로
올린다. 테스트: `a_topics_list_with_an_empty_prefix_is_refused_not_widened`.

### 8. `--tui`의 두 번째 Ctrl-C

리뷰 2 non-blocking. §5-15가 raw mode의 첫 Ctrl-C만 다루고 `signals.rs`가 주는 두 번째
보장(강제 종료)이 빠져 있었다.

TUI는 Ctrl-C를 세지 않는다 — 두 번째 `Intent::Cancel`은 호스트가 센다. `run.rs`의
`Reaction`이 그 판정이고(`Cancel`이 두 번째면 `ForceExit`), 그 자리에서 `signals.rs`와 같은
문장을 찍고 `std::process::exit(exit::CANCELLED)`한다. 터미널 복구는 `TerminalGuard`의
`Drop`이 아니라 명시적으로 한다 — `process::exit`은 소멸자를 돌리지 않는다.
테스트: `a_second_cancel_intent_forces_the_exit`.

### 9. `rivet plugin list`의 `DEFAULT` 열 — 넣지 않았다

리뷰 2 non-blocking이 스스로 "값이 있는지 한 번 더 볼 만하다"고 적은 그것이다. `ENABLED`
열이 이미 `catalog::select`의 결과를 찍으므로 설정 파일이 없는 트리에서 telemetry에 `no`를
준다. 두 열이 갈라지는 것은 명시적 `enabled` 목록이 있을 때뿐이고, 그때 운영자가 알고 싶은
것은 "내가 켰나"이지 "기본이 뭐였나"가 아니다. 대신 표 아래에 기본 선택 밖의 id를 **한 줄로**
적는다 — 열 하나를 모든 행에 늘리는 대신 그 정보가 필요한 한 곳에서만 말한다.

### 10. 상태바가 좁은 터미널에서 세그먼트를 통째로 버린다

설계에 없던 것. `the_three_panels_fit_an_eighty_column_terminal`을 쓰고 나서야 80칸에서
상태바가 `… 0 pl`로 잘리는 것을 봤다. 중간에서 잘린 상태바는 읽는 사람에게 아무것도 말하지
않고, 무언가 잘렸다는 사실까지 숨긴다. `status_line`이 폭을 받아 **세그먼트를 통째로**
버리도록 했고, 순위는 사용자가 없으면 일을 못 하는 것부터다 — 몇 번째 turn인지, 얼마나
들었는지, 어떻게 빠져나가는지.
