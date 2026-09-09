# Rivet 작업 계획

> 상태: Draft v0.3 (설계 리뷰 반영) · 갱신: Phase 0 완료 시점
> 설계 근거는 [`architecture.md`](./architecture.md)에 있다. 이 문서는 **무엇을 어떤
> 순서로 만들고, 무엇으로 완료를 판정하는가**만 다룬다.

---

## 0. 우선순위 원칙

가장 먼저 만들 것은 Plugin Marketplace가 아니다. 순서가 중요하다.

```text
Agent Loop → Session Event → Tool 계약 → Model 계약
  → Plugin Registry → Event Bus → TUI → Policy → Sandbox
  → 대화 → Job Runtime → External Plugin
```

특히 **`Agent Loop + Session + Event`를 먼저 안정화해야** 나머지를 안전하게 확장할 수
있다. 이 셋이 흔들리는 상태에서 Job Runtime을 얹으면 두 배로 고쳐야 한다.

> 이 줄은 Phase 0에서 처음 적혔고, 실제로 출시된 순서에 맞춰 고쳤다. 두 곳이 바뀌었다.
> `Plugin Registry`와 `Event Bus`가 자리를 맞바꾼 것은 단순한 사실 정정이다 — 레지스트리는
> Phase 2, 버스는 Phase 3이었다. 판단이 바뀐 것은 TUI 쪽이다: 원래는 TUI가 Job Runtime
> **뒤**에 있었는데 기본 TUI는 Phase 3에 왔고, Phase 4b가 그 화면을 대화형으로
> 만든 다음에야 Phase 5의 Job 패널이 안정된 수명 위에 지어진다. 순서가 중요하다는 원칙은
> 그대로이고, 바뀐 것은 무엇이 무엇에 기대는지에 대한 판단이다.

각 Phase는 **검증 가능한 완료 조건(DoD)** 을 가진다. "대충 됨"으로 다음 Phase에 넘어가지
않는다.

---

## Phase 0 — Repository ✅ 완료

| 항목 | 상태 |
|---|---|
| Cargo workspace (14 crates) | ✅ |
| `rivet-core` 계약 전체 정의 | ✅ |
| `BroadcastBus` (소유권 없는 lossy 버스) | ✅ |
| `Registry` (소유권 추적 등록소) | ✅ |
| CLI 명령 표면 | ✅ |
| CI (fmt · clippy · test · doc · audit) | ✅ |
| 설계 문서 5종 | ✅ |
| 독립 에이전트 설계 리뷰 + 반영 (2회전) | ✅ |

**DoD**

```bash
cargo fmt --all -- --check     # 차이 없음
cargo clippy --workspace --all-targets   # 경고 0
cargo test --workspace         # 135 tests, 0 failed
```

**Phase 0에서 이미 확정한 것** (재논의 대상 아님)

- 에러 분류가 재시도를 구동한다 (`ErrorKind::is_retryable`)
- Policy 합성은 most-restrictive-wins, 결정론적
- Session event(durable) ≠ Bus event(lossy)
- Context는 예산 인식, 항목 단위 폐기
- 등록은 소유권 추적, 이름 충돌 거부
- Job 종료 상태는 흡수, `RUNNING → COMPLETED` 직행 금지
- `Interceptor`는 허용을 넓힐 수 없고, 결과는 fold에 합류한다
- 승인은 durable session event다
- `ToolContext`는 직렬화 가능한 data와 live host로 분리한다
- 권한 교집합은 부분순서 위의 `meet`이다

### 설계 리뷰에서 고친 것

독립 에이전트 리뷰가 코드로 검증해 잡아낸 결함들. 전부 Phase 0에서 수정했고, 각각
회귀 테스트가 있다.

| 심각도 | 문제 | 수정 |
|---|---|---|
| Critical | `Interceptor`가 `Allow` 반환으로 Policy chain 우회 (게다가 이름순 첫 승) | `RestrictiveDecision` 도입, fold 합류, `priority()` |
| Critical | `ToolCalled` ↔ `ToolCompleted` 사이 크래시 시 replay가 provider 400 유발 | replay가 미해결 호출에 합성 error 결과 생성 |
| Critical | 승인이 lossy 버스에만 존재 → 감사 불가, resume 시 소실 | `approval.requested` / `approval.resolved` durable event |
| High | deny list가 glob·중첩·대소문자 모두 미처리 (`**/*.pem`이 아무것도 안 막음) | `GlobSet` + 대소문자 무시 + 문서 목록 회귀 테스트 |
| High | `intersect`가 정확 일치 → 좁게 선언한 plugin이 권한 0개 | `Permission::meet` 부분순서 |
| High | `ToolContext`가 `Arc<dyn ...>`를 들어 Phase 6 이전 불가 | `ToolContextData` + `ToolHost` 분리 |
| High | `Job.state`가 pub → 한 줄로 리뷰 게이트 무력화 | private + `JobGraph::apply()` |
| High | `is_settled()`가 `WAITING`을 종료로 간주 | `WAITING`을 live로 계산 |
| High | 등록만 되고 조회 불가한 슬롯, 언로드 후에도 살아있는 구독자 태스크 | 접근자 추가 + 소유자별 `JoinHandle` abort |
| High | `append`의 내구성 계약 부재, `Option<u64>` 탈출구 | fsync 계약 명시 + `Expect` enum |
| Medium | `count_tokens`가 tool 출력을 0으로 계산 | `billable_len()` 전 블록 계산 |
| Medium | `bytes/4`가 한국어를 2~4배 과소 추정 | 문자 기반 + non-ASCII 가중 |
| Medium | `fit_to_budget` 우선순위 역전 + key 중복 미제거 | 엄격 밴드 + dedupe |
| Medium | Sandbox가 `Drop`으로 해제한다는 지킬 수 없는 계약 | 런타임의 `teardown` 의무로 변경 |
| Medium | Checkpoint 요약이 필드에만 남아 소실 가능 | 요약을 메시지로 투영 |

**2회전** — 1회전 수정을 같은 에이전트가 적대적으로 재검증했다. 수정이 만든 회귀 5건과
불완전했던 수정 7건이 나왔고, 전부 고쳤다.

| 심각도 | 문제 | 수정 |
|---|---|---|
| Critical | `Modify`가 severity에 섞여 있어 ①좁힌 rewrite가 승인 요구에 삼켜지고 ②무제약 `Allow`가 다른 정책의 sandbox 요구를 지움 | `PolicyDecision`을 `outcome` / `rewrite` / `constraints` 세 필드로 분리, constraints는 축별 최소값으로 merge |
| Critical | `FsScope::Subtree("../../../etc")`가 `Workspace` 프로파일과 meet하여 워크스페이스 밖 권한이 됨 | `is_contained()` 검증 + `FsScope::subtree()` 생성자 |
| High | glob 전환이 디렉터리 봉쇄를 회귀시킴 — `.ssh`는 막고 `.ssh/id_rsa`는 통과 | `expand()`가 `p/**`와 `**/p/**`도 생성 |
| High | 재시도 예산 강제가 `READY` 교착 생성 — 소진된 Job이 어디로도 못 감 | `(Ready, Failed)` 전이 추가 |
| High | `remembered_approvals`가 `ToolCallId` 키라 영원히 매칭 안 됨 | `scope_key` 도입, `is_pre_approved()` |
| High | `derive(Deserialize)`가 Job 불변식 우회 — JSON으로 `COMPLETED`·순환 주입 가능 | `JobGraph::from_jobs()` 경유 커스텀 `Deserialize` |
| High | Checkpoint가 `pending_tool_calls`를 안 비워 선행 호출 없는 tool result 생성 | Checkpoint에서 clear |
| High | dedupe가 우선순위 무시 — 같은 key의 `Optional`이 `Required` 시스템 프롬프트를 축출 | 우선순위 높은 쪽을 남김 |
| Medium | `count_tokens`가 여전히 `billable_len()/4` (바이트 기반) | `Message::estimated_tokens()` 문자 기반 |
| Medium | `register_subscriber`가 attach하지 않아 이벤트가 안 흐름 | 등록이 곧 attach |
| Medium | `Modify`가 호출을 임의 rewrite 가능 | `rewrite_is_wellformed()` + rewrite 후 파이프라인 재평가 계약 |
| Low | `Event` enum이 `PolicyDecision` 인라인으로 비대 | `Box<PolicyDecision>` |

---

## Phase 1 — Minimal Agent

**목표**: 실제 모델과 대화하고 파일을 읽는 에이전트가 동작한다.

```bash
rivet "list the files here and explain what this project does"
```

### 작업 항목

| # | 작업 | crate | 의존 |
|---|---|---|---|
| 1.1 | JSONL Session store (`append` + `Expect` + fsync + `read` + `fork`) | `rivet-session` | — |
| 1.2 | `ContextAssembler` (providers → `fit_to_budget` → `ModelRequest`) | `rivet-runtime` | — |
| 1.3 | `AgentLoop` (turn 루프 · 한도 5축 · 취소) | `rivet-runtime` | 1.1 1.2 |
| 1.4 | `ToolDispatcher` (resolve → scope → validate → execute → persist) | `rivet-runtime` | 1.1 |
| 1.5 | OpenAI 호환 어댑터 (SSE 스트리밍 · tool call 조립 · 에러 분류) | `plugins/model-openai` | — |
| 1.6 | `read_file` `write_file` `list_dir` `search` | `plugins/tool-filesystem` | — |
| 1.6b | **post-open 경로 재검증** (`O_NOFOLLOW` / canonicalize 후 재검사) | `rivet-runtime` | 1.6 |
| 1.7 | `SystemPromptProvider` `WorkspaceProvider` | `rivet-runtime` | 1.2 |
| 1.8 | CLI 실행 경로 + JSONL 출력 모드 | `rivet-cli` | 1.3 |
| 1.9 | `rivet.toml` 로딩 + 프로파일 | `rivet-cli` | — |

### 위험 요소

- **1.5가 가장 위험하다.** provider별 SSE 방언, tool call 인자의 부분 JSON 조립, 에러
  코드 → `ErrorKind` 매핑이 전부 여기 모인다. 먼저 착수하고 record/replay 픽스처로
  테스트를 고정한다.
- **1.3의 취소 처리**. `CancellationToken`이 도구·HTTP·프로세스까지 전파되지 않으면
  Ctrl-C 후 `cargo build`가 고아로 남는다.
- **1.6b는 1.6과 같은 PR에 있어야 한다.** 원래 계획은 이것을 Phase 4에 두었는데, 그러면
  파일을 여는 도구가 Phase 1에 출시되고 Phase 4까지 워크스페이스 봉쇄가 **형식적으로만**
  존재한다. `Workspace::resolve()`는 어휘적 검사일 뿐이고, 워크스페이스 안의 심볼릭 링크가
  밖을 가리키면 통과한다. 파일을 여는 첫 커밋과 재검증은 분리될 수 없다.

### DoD

- [x] 실제 provider로 왕복 대화가 동작
      (수동 검증: `rivet "list the files here and explain what this project does"` →
      `list_dir` → `read_file` → 모델이 두 결과를 모두 사용해 답변, 3턴 `done`,
      3384 in / 581 out 토큰. 세션 `ses_01a07637`)
- [x] tool call → 파일 읽기 → 모델이 결과를 사용
      (`rivet-cli/tests/e2e.rs::a_prompt_runs_a_tool_and_the_model_sees_its_result`:
      두 번째 요청의 메시지 배열에 파일 내용이 담긴 tool result가 있음을 단언)
- [x] Ctrl-C가 5초 안에 in-flight 요청과 도구를 정지
      (`rivet-cli/tests/e2e.rs::ctrl_c_stops_an_in_flight_tool_and_exits_130` — 실제
      SIGINT를 받은 서브프로세스가 130으로, 신호 시점부터 5초 안에 종료함을 단언)
- [x] `rivet resume <session>`이 로그 replay로 이어감
      (`rivet-runtime/tests/session_recovery.rs`, `rivet-cli/tests/e2e.rs`)
- [x] 5개 한도가 각각 발동하는 테스트 존재
      (`rivet-runtime/tests/agent_loop.rs`의 `the_*_limit_trips` 5개, 각각 어느
      `LimitKind`인지까지 단언)
- [x] provider 응답 픽스처 기반 offline 테스트 통과
      (`plugins/model-openai/tests/fixtures/*.sse` 11개 + 디코드·인코딩·에러 분류·루프백)
- [x] 심볼릭 링크 탈출 시도가 차단됨 (1.6b)
      (`rivet-runtime/tests/fsguard.rs`, unix 한정 — windows는 미검증)
- [x] `kill -9` 후 재개 시 미완 tool call이 합성 결과로 닫힘
      (`rivet-cli/tests/e2e.rs::a_killed_run_is_closed_with_a_synthetic_result_and_resumes`
      — 실제 SIGKILL)

---

## Phase 2 — Plugin

**목표**: 기능이 crate 경계 밖에서 등록된다.

```bash
rivet plugin list
rivet plugin show rivet.tool-filesystem
```

| # | 작업 | crate |
|---|---|---|
| 2.1 | `rivet-plugin.toml` 파싱 + 검증 | `rivet-plugin` |
| 2.2 | `PluginLoader` (discover → validate → load → register → active) | `rivet-plugin` |
| 2.3 | ABI 호환 검사 (`CapabilityVersion::accepts`) 로 **등록 전** 거부 | `rivet-plugin` |
| 2.4 | 로드 실패 롤백 (`unregister_all`) | `rivet-plugin` |
| 2.5 | 권한 교집합 계산 (manifest ∩ profile) | `rivet-plugin` |
| 2.6 | 기존 도구들을 Plugin으로 재포장 | `plugins/*` |
| 2.7 | `rivet plugin new` 스캐폴딩 | `rivet-cli` |

### DoD

각 항목 뒤는 그것을 증명하는 테스트 이름이다.

- [x] 부분 등록 후 실패한 plugin이 **아무것도** 남기지 않음 (테스트)
      — `a_plugin_that_fails_after_registering_leaves_nothing`,
      `a_plugin_that_panics_after_registering_leaves_nothing`,
      `a_registration_from_a_task_outliving_a_failed_load_is_refused`,
      `a_registration_from_a_task_outliving_a_successful_load_is_refused` (롤백은 한 시점의
      청소가 아니라 봉인이다 — plugin이 계속 들고 있는 guard로 나중에 등록하는 것도
      거부된다. 그게 아니면 "아무것도"는 "`load` 안에서 동기적으로 등록한 것은
      아무것도"라는 뜻이 된다. 성공한 load도 같은 자리에서 봉인되며, 그쪽은 인스턴스가
      살아 있어 피해가 더 조용하다 — 망가지는 것은 등록 회계다)
- [x] ABI 불일치 plugin이 등록 시도 전에 거부됨
      — `an_incompatible_abi_is_rejected_before_load_is_called` (spy가 `load` 진입을
      기록하고, 그 플래그가 false임을 확인한다)
- [x] `readonly` 프로파일이 쓰기 plugin의 쓰기 권한을 실제로 제거
      — `a_readonly_profile_strips_write_permission` (로더),
      `a_grant_without_write_does_not_offer_a_write_tool` (plugin),
      `a_readonly_profile_leaves_write_file_unregistered` (e2e)
- [x] 이름 충돌 시 양쪽 plugin 이름이 에러에 나옴 — `a_name_collision_names_both_plugins`
- [x] unload 후 같은 이름 재등록 성공 (hot reload 전제)
      — `a_plugin_reloads_under_the_same_name`,
      `a_plugin_that_registers_from_unload_does_not_break_its_own_reload` (plugin 자신의
      teardown이 이름을 다시 잡아 재로드를 막지 못한다)

미검증으로 남긴 것: `Plugin::load`/`unload`가 **매달리는** 경우에 타임아웃이 없다.
등록 창구가 생기면서 범위가 넓어졌다 — `seal`은 진행 중인 등록을 기다려 내고 등록은
`tool.spec()`을 창구 안에서 부르므로, plugin이 띄운 태스크의 등록 하나가 멈추면 `load`도
멈춘다 (리뷰 3라운드 측정: `spec()`이 2 s 블로킹 → `load`가 ~50 ms 대신 4.007 s. "영영
안 돌아옴"이면 `rivet run`이 시작 지점에서 메시지도 `FAILED` 레코드도 없이 매달린다).
그러니 정확히는 **`load`·`unload`, 그리고 그것들이 띄운 태스크가 진행 중인 등록**에
타임아웃이 없다. in-process plugin 셋은 모두 신뢰 대상이라 위험이 낮지만, 강제되는 것은
없다. 데드라인이 누구 몫인지는 `architecture.md` §11-15.

---

## Phase 3 — Event

**목표**: plugin이 루프를 수정하지 않고 실행을 관찰한다.

| # | 작업 | crate |
|---|---|---|
| 3.1 | 전체 이벤트를 실제 발행 지점에 연결 | `rivet-runtime` |
| 3.2 | `EventSubscriber` 등록 + 토픽 필터 | `rivet-runtime` (등록) · `rivet-plugin` (강제) |
| 3.3 | `telemetry.log` plugin (구조화 로그) | `plugins/` |
| 3.4 | JSONL 이벤트 스트림 (`--jsonl`) | `rivet-cli` |
| 3.5 | 기본 TUI (Job 패널 · Agent 패널 · 상태바) | `rivet-tui` |

### DoD

각 항목 뒤는 그것을 증명하는 테스트 이름이다.

- [x] TUI가 런타임 타입을 **하나도** import 하지 않고 이벤트만 소비
      — `the_tui_crate_does_not_depend_on_the_runtime` (`rivet-tui`: 자기 `Cargo.toml`을
      `toml`로 **파싱**해 `[dependencies]`·`[dev-dependencies]` 어디에도 `rivet-runtime`이
      없고 rivet 의존이 `rivet-core` 하나임을 단언한다. 의존이 없으면
      `use rivet_runtime::…`은 컴파일되지 않으므로 이 테스트가 지키는 것은 성질이 아니라
      그 성질을 되돌리는 편집이다. 문자열 검색이 아닌 이유: 주석에 오탐하고
      `rivet-runtime.workspace = true` 표기를 놓친다),
      `every_panel_is_filled_from_events_alone` (양의 증명 — 다섯 패밀리의 envelope를
      손으로 만들어 fold하고 Job 패널·Agent 패널·상태바를 전부 단언한다. 런타임도, 버스도,
      터미널도 없이),
      `the_status_bar_shows_only_what_events_carry` (음의 단언 — 어떤 이벤트도 나르지 않는
      프로파일 이름·워크스페이스 루트는 상태바에 **없다**)
- [x] 느린 구독자가 루프를 지연시키지 않음 (측정)
      — `a_slow_subscriber_does_not_delay_the_loop` (`rivet-runtime/tests/event_flow.rs`:
      버스 용량 16, `on_event`가 이벤트당 20 ms 자는 구독자, 델타 400개를 흘리는
      `FixtureModel`로 **실제 `AgentLoop`를 돌린다**. 직렬 배달이었다면 ≥ 8 s인데 run이
      1 s 안에 끝남을 단언한다 — 여유 8배, 상한은 루프에만 걸고 구독자에는 안 건다)
- [x] `SubscriberLagged`가 실제로 발행됨
      — `a_lagging_subscriber_is_reported_by_name_on_the_bus` (용량 8, `on_event` 안에
      멈춰 있는 구독자 하나 + 기록자 하나. 이벤트 40개를 발행하고 게이트를 연 뒤, 기록자가
      `subscriber: "wedged"`인 `runtime.subscriber.lagged`를 `dropped > 0`으로 받았음을
      단언한다. 기존 `a_slow_subscriber_lags_instead_of_stalling_the_publisher`는 수신자의
      `RecvError`만 봤지 발행된 이벤트를 보지 않았다),
      `a_lag_report_does_not_feed_itself_into_a_runaway` (보고가 **구간당 하나**임 — 결과당
      하나로 보고하면 보고 자체가 다음 랙을 만든다. §"구현 중 고친 것" 참조)
- [x] 등록된 subscriber plugin이 실제로 이벤트를 받음 (`attach_subscriber`)
      — `a_registered_subscriber_plugin_receives_events`
      (`rivet-plugin/tests/subscriber.rs`: 진짜 `PluginLoader`로 `event_subscriber` +
      `events_subscribe`를 선언한 plugin을 로드하고, 버스에 발행하고, plugin의 sink가
      봤음을 단언한다), 짝: `the_telemetry_plugin_logs_what_it_receives` ·
      `every_log_record_names_its_topic_and_its_run` (`plugins/telemetry-log`: `tracing`
      테스트 레이어로 필드까지 본다)
- [x] 언로드된 plugin의 구독 태스크가 중단됨
      — `an_unloaded_plugins_subscription_task_stops` (로더의 `unload`를 지나는 경로로,
      unload 전 관측 / unload 후 무관측),
      `an_unloaded_subscriber_is_dropped_not_merely_silenced` (구독자에 `Drop` 플래그를
      달아 펌프가 들고 있던 `Arc`가 실제로 떨어졌음을 본다 — "배달이 멈췄다"와 "태스크가
      끝났다"는 다른 주장이고, DoD가 말하는 것은 후자다)
- [x] `--jsonl`이 관찰 가능성을 제공 — **세션 재구성용이 아님**
      — 두 줄이므로 두 테스트다.
      `jsonl_carries_the_whole_lifecycle_not_just_the_answer`
      (`rivet-cli/tests/e2e.rs`: 한 번의 실행에서 `runtime.started` ·
      `plugin.discovered` · `plugin.loaded` · `agent.run.started` ·
      `tool.execute.started` · `tool.execute.completed` · `agent.run.completed` ·
      `runtime.shutting_down` · `plugin.unloaded`가 전부 스트림에 있고 `runtime.started`가
      **첫 줄**임을 단언한다),
      `the_jsonl_stream_is_not_a_session_export` (같은 세션에 대해 `--jsonl` 출력에는
      `user.message`·`assistant.message`·`seq`가 **하나도 없고** `rivet session show --json`
      에는 셋 다 있음을 단언한다 — durable fact는 버스에 없다)

> 원래 DoD는 "`--jsonl` 출력만으로 세션 재구성 가능"이었다. 이것은 버스가 lossy라는
> 설계와 모순이다. 유실될 수 있는 스트림으로 durable 로그를 재구성할 수는 없다.
> 세션 재구성이 필요하면 세션 로그를 export 해야 한다 (`rivet session show --json`).

### 작업 항목별 증명

- **3.1 전체 이벤트를 발행 지점에 연결** — `every_bus_topic_is_claimed`이 28개 토픽을
  **와일드카드 없는 두 층 `match`**로 20개(발행됨)와 8개(Phase 4의 셋 · Phase 5의 다섯)로
  갈랐다 — Phase 4가 그 셋을 발행하면서 23 대 5가 됐고, 그 편집을 강제한 것이 이 tripwire다. 토픽이나 패밀리가 새로 생기면 컴파일에 실패한다. 실제 발행은
  `a_run_publishes_every_agent_and_tool_topic_this_phase_owns`(13개를 한 대본으로)와
  `a_host_lifecycle_publishes_every_runtime_and_plugin_topic_this_phase_owns`,
  `runtime_started_precedes_everything_it_would_describe`가 본다. `runtime.started`·
  `runtime.shutting_down`은 신규 — `rivet_runtime::lifecycle`.
- **3.2 `EventSubscriber` 등록 + 토픽 필터** — `rivet-plugin/tests/subscriber.rs` 14개.
  빈 목록이 grant가 되는 것, 긴 접두사가 이기는 것, 겹치지 않으면 거부되는 것, 두 grant가
  join되는 것, 이름이 래퍼를 통과하는 것, 슬롯 검사와 권한 검사가 서로 다른 거부인 것.
  `architecture.md` §11-10을 닫는 테스트는
  `a_readonly_profile_keeps_the_conversation_from_a_subscriber`.
  **강제는 `rivet-plugin`의 guard에 있다** (이 표는 `rivet-runtime`으로 적고 있었다):
  grant가 이미 거기 있고, `Registry::scoped`에 grant 인자를 더하는 것은 모든 embedder에
  대해 Phase 0 계약을 바꾸는 일이다. 등록 경로(`attach_subscriber`)는 런타임에 남는다.
- **3.3 `telemetry.log` plugin** — id는 `rivet.telemetry-log`(아무도 소유하지 않는
  `telemetry` namespace를 만들지 않는다). `the_default_topics_survive_every_shipped_profile` ·
  `a_readonly_profile_refuses_a_telemetry_plugin_that_was_told_to_log_the_conversation` ·
  `configured_topics_the_profile_narrows_are_refused_not_silently_dropped` ·
  `include_conversation_off_means_it_does_not_even_subscribe` ·
  `the_telemetry_plugin_logs_a_run_when_enabled` (e2e) ·
  `the_telemetry_plugin_is_not_in_the_default_selection` (e2e).
- **3.4 JSONL 이벤트 스트림** — DoD 6의 둘에 더해
  `a_truncated_drain_is_reported_not_swallowed`(`sleep(20 ms); abort()`를 대신한
  `drain_within`이 잘린 것을 **말한다**), 회귀 감시로 기존
  `jsonl_output_is_one_parseable_event_per_line`, 그리고
  `an_opt_in_plugin_is_absent_by_default_and_loads_when_named` ·
  `every_default_selection_id_is_in_the_catalog` ·
  이름을 고친 `with_no_config_file_every_plugin_the_default_selection_names_is_loaded`.
- **3.5 기본 TUI** — `the_agent_panel_follows_a_run_from_start_to_stop` ·
  `the_job_panel_renders_what_job_events_carry`(job 런타임 없이 `job.*` envelope만으로) ·
  `the_job_panel_says_where_jobs_come_from_when_empty` ·
  `the_status_bar_counts_drops_from_the_lag_report` ·
  `the_three_panels_fit_an_eighty_column_terminal`(`TestBackend` 스냅샷) ·
  `quitting_asks_the_host_rather_than_reaching_for_the_token` ·
  `ctrl_c_in_the_tui_asks_for_a_cancel` + `a_second_cancel_intent_forces_the_exit`
  (raw mode가 SIGINT를 삼키므로 Phase 1의 **두 겹** 보장을 TUI 안에서 다시 만든다) ·
  `tui_refuses_a_pipe`(e2e).

### 구현 중 고친 것 — 랙 보고가 자기 자신을 먹여 살렸다

설계는 "보고는 `Lagged` 결과 하나당 하나이고 되먹임 폭주는 없다"고 적었다. 틀렸다. 보고
자체가 `publish`이고, 꽉 찬 채널로의 `publish`는 가장 오래된 슬롯을 덮어쓰는데 `Lagged`
직후 tokio가 수신자를 재배치하는 자리가 정확히 그 슬롯이다. **측정: 용량 8 버스에 뒤처진
구독자 둘, 발행 40개가 118,312개가 될 때까지 아무도 이벤트를 하나도 못 받았다.** DoD 2·3의
랙 단언이 둘 다 이것 때문에 처음에 실패했다.

`pump`가 이제 **유실 구간당 한 번만** 보고한다. 구간은 무언가 실제로 배달됐을 때 닫히고,
그 사이의 드롭은 보고되지 않는다 — 그것들은 그 보고가 만든 드롭이다. Phase 0부터 `attach`에
있던 성질이고, 얕은 버스에 뒤처진 구독자를 둘 올린 적이 없어서 안 드러났다.

### 미검증으로 남긴 것

`--tui`의 **실제 raw mode 경로**는 자동화 테스트가 없다. 파이프 거부(`tui_refuses_a_pipe`),
키 매핑(`ctrl_c_in_the_tui_asks_for_a_cancel` · `a_second_cancel_intent_forces_the_exit`),
그리기(`TestBackend` 스냅샷)는 전부 테스트가 있지만, "진짜 터미널에서 raw mode에 들어갔다
나온다"는 pty가 필요하고 Phase 3은 그것을 도입하지 않았다. `TerminalGuard`의 `Drop` + 패닉
훅이 복구를 맡는다.

`Plugin::load`/`unload`의 **타임아웃은 여전히 없다** (Phase 2가 남긴 것). Phase 3은 그것을
고치지 않고 **보이게** 만들었다 — 관측자가 로드 **전에** 붙으므로 매달린 `load`가
"`runtime.started`와 `plugin.discovered`는 있는데 `plugin.loaded`도 `plugin.load.failed`도
없는 스트림"으로 드러난다
(`a_load_that_hangs_shows_up_as_a_discovered_plugin_that_never_loaded`). 진단이 생겼지
데드라인이 생긴 것은 아니다. 데드라인이 누구 몫인지는 `architecture.md` §11-15.

`events_publish`는 강제되지 않는다. `ctx.events`가 grant와 무관하게 넘어가므로 plugin이
위조 `agent.*`를 발행할 수 있다. 모든 프로파일이 이 권한을 주므로 오늘 아무것도 안 터지고,
진짜 답은 발행자를 envelope에 스탬프하는 것이며 그건 Phase 6의 모양이다.

설계 문서: [`design/phase-3-event.md`](./design/phase-3-event.md).

---

## Phase 4 — Policy / Sandbox

**목표**: 위험한 도구가 판단을 거친다.

```text
unsafe tool → policy → approval | deny → sandbox → execution
```

| # | 작업 | crate |
|---|---|---|
| 4.1 | Policy chain fold + `unattended` 변환 | `rivet-runtime` |
| 4.2 | `Interceptor` 실행 (타임아웃 포함) | `rivet-runtime` |
| 4.3 | 기본 정책 (워크스페이스 봉쇄 · 파괴적 명령 게이트) | `plugins/policy-default` |
| 4.4 | 승인 UI + "세션 동안 기억" | `rivet-tui` `rivet-cli` |
| 4.5 | `sandbox-local` (프로세스 그룹 kill · 출력 상한 · 빈 env) | `plugins/sandbox-local` |
| 4.6 | `shell` 도구 (샌드박스 경유) | `plugins/tool-shell` |
| 4.7 | `git` 도구 (status · diff · log · commit) | `plugins/tool-git` |
| 4.8 | (Phase 1.6b로 이동) | — |
| 4.9 | 프로파일 5종 (`developer` `readonly` `reviewer` `ci` `production`) | `rivet-cli` |

### 위험 요소

- **4.5의 프로세스 트리 종료.** 취소 시 자식 프로세스가 고아로 남으면 안 된다.
  `SandboxHandle`은 `Drop`으로 해제하지 않는다 — Rust `Drop`은 await할 수 없다. 런타임이
  취소·패닉 경로를 포함해 **모든** 경로에서 `teardown()`을 호출해야 한다.
- **4.4의 승인 기록.** 승인 결과는 버스가 아니라 세션 로그에 남아야 한다.

### DoD

- [x] `--headless`에서 승인 요구가 매달리지 않고 거부됨
      (`a_headless_run_denies_an_approval_instead_of_waiting` — sink가 불리면 `panic!`한다 ·
      e2e `headless_refuses_a_destructive_command_without_hanging`)
- [x] 승인/거부가 세션 로그에 durable event로 남음
      (`an_approval_leaves_a_requested_and_a_resolved_in_the_log` · 버스 쪽은
      `the_bus_carries_both_approval_topics`)
- [x] resume 후에도 "세션 동안 기억" 승인이 유지됨
      (`a_replayed_grant_skips_the_sink_on_the_next_run` — 로그를 replay해 만든
      `SessionState`로 두 번째 run을 시작하고, 그 run의 sink는 불리면 `panic!`한다)
- [x] 취소 시 프로세스 트리 전체 종료 (좀비 없음)
      (`cancelling_kills_the_whole_process_tree` · 소유권 쪽은
      `an_abandoned_tool_task_still_gets_its_sandbox_torn_down`)
- [x] 거부된 도구가 세션에 `ToolBlocked`로 남고 모델이 사유를 봄
      (`a_denied_call_is_blocked_and_the_model_reads_the_reason`)
- [x] `readonly` 프로파일에서 `write_file`이 실제로 거부됨 — 두 층이므로 두 테스트다.
      등록층은 Phase 2의 `a_readonly_profile_leaves_write_file_unregistered`,
      정책층은 `a_readonly_profile_denies_a_write_even_if_the_tool_is_registered`
      (로더를 지나지 않고 레지스트리에 직접 넣은 쓰기 도구를 `default.grant`가 막는다)

설계: [`design/phase-4-policy-sandbox.md`](./design/phase-4-policy-sandbox.md).

---

## Phase 4b — Conversation

**목표**: 화면이 run 하나보다 오래 산다.

```text
prompt → run → [화면 유지] → prompt → run → … → q
```

**왜 정수 Phase가 아닌가.** `Phase 5`는 사용자가 읽는 문자열로 **네 곳**에 박혀 있다 —
`main.rs:275`(`--job`), `main.rs:320`(`rivet job`), `draw.rs:21`의 `NO_JOBS`, 그리고
`doctor.rs:76`("read, but the job runtime lands in Phase 5", `config.md:479`가 그대로
옮겨 적었다). `panels.rs:162-165`는 화면에 `"Phase 5"`가 있는지를 **단언**하므로 번호를 밀면
그 테스트가 깨지고, `event_flow.rs:169`의 "five topics wait on Phase 5"는 깨지지는 않고
거짓이 된다. 얻는 것이 없으므로 번호를 밀지 않는다.

**왜 Phase 5 앞인가.** 두 가지다. 5.6(TUI Job 패널)이 이 화면 *위에* 지어지므로, 화면이 아직
run과 함께 죽는 상태에서 만들면 곧 바뀔 수명을 가정하고 짓게 된다. 그리고 Phase 5의 DoD는
"사람 개입 없이 완주"(unattended)인데 대화형 화면은 반대 축(attended)이다. 한 Phase에 두 축을
섞으면 어느 쪽도 증명하지 못한다.

| # | 작업 | crate |
|---|---|---|
| 4b.1 | `Intent::Prompt(String)` — 화면에서 호스트로 가는 둘째 왕복, 유실 없는 전달 | `rivet-tui` |
| 4b.2 | 입력 모드(`Watching` / `Composing`)와 컴포저 | `rivet-tui` |
| 4b.3 | `AgentEvent::InputReceived` + `agent.input.received` 토픽 | `rivet-core` `rivet-runtime` |
| 4b.4 | 새 토픽의 소비자를 맞춘다 — 컴파일이 깨지는 둘(`rivet-tui` · `telemetry-log`)과 조용한 둘(`--jsonl` · `--human`) | `rivet-cli` `rivet-tui` `plugins/telemetry-log` |
| 4b.5 | transcript — 사용자 turn을 fold로 그리고 run 단위 필드를 turn 경계에서 정리 | `rivet-tui` |
| 4b.6 | `drive()`를 turn 루프로 — teardown은 루프 밖, **수리·취소 토큰·취소 플래그는 turn마다** | `rivet-cli` |
| 4b.7 | `answer()` 재출력 삭제 + `report()`를 화면 밖으로 + 잘린 앞부분을 알리던 사과의 새 자리 | `rivet-cli` `rivet-tui` |
| 4b.8 | 문서 갱신 — 산문 **열다섯 곳**이 이날 손봐야 한다 ([표](#4b8--이-phase가-끝나는-날-거짓이-되는-문장)) | `docs/` `README.md` `rivet.example.toml` |

### 결정 — turn 사이의 상태는 로그에서 다시 읽는다

두 길이 있었다.

- **A. 로그 replay.** `Intent::Prompt`만 더하고, turn 사이에 `SessionState`를 store에서 다시
  만든다.
- **B. `AgentLoop::run`이 `(RunSummary, SessionState)`를 돌려준다.**

**A를 택한다.** B는 "지금 상태가 무엇인가"에 대한 **둘째 출처**를 만든다. 둘이 갈라지면 옳은
쪽은 언제나 로그이고, `architecture.md` §4.6이 "세션은 append-only 이벤트 로그이며 단일 진실
원천"이라고 이미 정해 놓았다. 덤도 있다: **replay 불변식이 매 turn 실행된다.** 오늘
`SessionState::replay`가 순수하고 전체적이라는 성질은 크래시 뒤 `resume`에서만 통과하는데,
대화형 TUI는 그것을 대화 한 번에 N번 지난다.

**그런데 turn 루프가 반복할 것은 `started()`의 한 줄이 아니라 `resume()`의 경로다.** 이 구분이
이 항목에서 가장 틀리기 쉬운 지점이므로 이유를 적어 둔다.

**1. 수리를 건너뛰면 대화가 세션을 영구히 망가뜨린다.** `session_recovery.rs:39-50`이 바로 이
패턴을 금지한다 — `SessionState::replay`는 매달린 호출을 **메모리에서만, 그리고 로그가 거기서
끝날 때만 옳게** 닫는다. 문서가 그린 그림이 그대로 우리 경우다:

```text
[user, assistant(tool_calls A), assistant(text), tool_result(A)]
```

끝에 고아 tool 메시지가 남고, 그것이 이 모듈이 막으려고 존재하는 400이다. 가상이 아니다 —
`agent_loop.rs:330`(`MaxTokens`)과 `:337`(`Refusal`)은 `AssistantMessage`를 **적은 뒤**
`close_remaining` 없이 반환한다(그 함수는 `:663`의 도구 경로 안에서만 불린다). 잘린 assistant
메시지도 온전한 `ContentBlock::ToolCall`을 들고 있을 수 있고, `tool.called`는 적히지 않았으므로
`pending_tool_calls`는 비어 있다(유일한 기록자가 `session.rs:338`이다). 그 다음 turn이
`UserMessage`를 그 뒤에 붙이면 `inspect`가 `Rli::Broken`을 돌려주고, 그때부터 `close_interrupted`가 `Err`를 돌려주고 `rivet resume`은 **영구히** 거절하며 fork하라고
말한다(`session_recovery.rs:177-183`).

그래서 **turn마다 `close_interrupted`를 지난다.** 건강한 세션에서는 공짜다 — `Rli::Satisfied`면
아무것도 쓰지 않고 `SessionState::replay(&events)`를 그대로 돌려준다(`session_recovery.rs:175`).
멱등이므로 매 turn 불러도 안전하고, 안 부르면 위의 경로가 열린다.

**2. `run.rs:81`의 1,000은 turn 루프에서 조용한 절단이다.** `store.read(session_id, 1, 1_000)`은
`started()`에서는 무해하다(로그에 이벤트가 하나뿐이다). 매 turn 반복하면 1,000 이벤트를 넘긴
대화가 **앞부분만** replay되고, `state.last_seq`가 낡아 `SessionWriter::at_state`의 `Expect::Seq`가
충돌하며, 모델은 자기 대화의 가운데를 잃는다. 옳은 쪽은 `read_all`이다 — `READ_PAGE = 10_000`으로
끝까지 페이징한다(`session_recovery.rs:249-266`). 두 경로는 바꿔 쓸 수 있는 것이 아니다.

값은 정직하게 적는다: turn마다 O(n) 읽기이므로 세션 전체로는 O(n²)다. 사이에 모델 왕복이 있어
실측상 묻힐 것이나, 묻힌다는 것과 없다는 것은 다르다. 그리고 **읽기 비용은 이 문제의 작은
쪽이다** — 아래 위험 요소의 컨텍스트 항목을 볼 것.

### 결정 — 사용자 turn은 이벤트로 온다, 로컬 에코가 아니라

`SessionEvent::UserMessage`는 **이미 기록된다**(`agent_loop.rs:199-206`). durable한 절반은
있고, 없는 것은 관찰 쪽 쌍둥이뿐이다 — `events.md`가 그리는 이중 채널의 빈칸이다.

로컬 에코는 `AppState::apply`가 순수 fold라는 성질에 **둘째 예외**를 뚫는다. 첫째 예외
(`pending`)는 `lib.rs:25`가 근거를 적어 정당화했다 — "승인은 관찰이 아니라 왕복이고, 버스는
한 방향이라 답을 놓을 데가 없다." 사용자 입력에는 그 논증이 **적용되지 않는다**: 답을 놓을
데는 이미 있다(세션 로그). `lib.rs:18-21`이 더 직접적으로 말한다 — "필요한 것이 있으면 import가
아니라 **이벤트를 더하라**."

그리고 이 토픽은 이미 예고돼 있다. `config.rs:329-332`:

> a topic added under `agent.` later — **a prompt echo, say** — is **not** granted until
> somebody adds it here, so the list fails closed rather than widening on its own.

즉 `agent.input.received`는 `readonly`·`reviewer`·`production`에 **닫힌 채로** 도착하고
(일곱 prefix 어디에도 걸리지 않는다, `config.rs:339-345`), 컴파일 트립와이어 둘이 그 닫힘을
우연이 아니라 **답변**으로 만든다: `every_bus_topic_is_claimed`(`event_flow.rs:77-84`)과
`every_topic_is_granted_or_deliberately_withheld`(`config.rs:689-697`). 둘 다 와일드카드가 없고,
`one_of_each!`가 샘플까지 함께 강제한다. 후자의 답은 `false`여야 하며 이유는 `TextDelta`와 같다.

**변이 하나를 더하는 값은 `rivet-core` 밖에서도 치른다. 그리고 그 값은 두 종류다.**

*컴파일이 깨지는 곳 둘.* `plugins/telemetry-log`의 `agent_detail`(`record.rs:118-162`)과
`rivet-tui`의 `AppState::apply`(`state.rs:183-214`)가 둘 다 와일드카드 없는 `match`다 —
`state.rs`에는 `_ =>`가 **하나도** 없다. 두 crate가 컴파일에 실패하고, 그것이 이 설계가
원하는 바다.

*조용히 지나가는 곳 둘.* `--jsonl`은 변이별로 고칠 것이 **없다** — `JsonlRenderer`는
`serde_json::to_string(envelope)` 한 줄이고(`render/jsonl.rs:30-36`) `AgentEvent` `match`가
아예 없다. 새 토픽은 공짜로 실린다. 문제는 공짜라는 것이다: 사용자가 친 프롬프트가 그 순간
stdout으로 나가기 시작하는데, 아무도 그렇게 정한 적이 없다. `--human`은 `_ => {}`라
(`render/human.rs:86`) 말없이 무시한다. 둘 다 틀리지는 않았고, 둘 다 **아무도 결정하지 않은
채 바뀐다.** 컴파일러가 잡아 주지 않으므로 4b.4가 명시적으로 답해야 한다.

같은 crate에서 고칠 것이 하나 더 있다 —
`CONVERSATION_TOPIC`은 `"agent.text."` 하나이고(`telemetry-log/src/lib.rs:73`), 게이트는 그 하나에
닿는 prefix만 거절한다(`lib.rs:176-192`). 손대지 않으면 `include_conversation = true`가 모델의
절반만 덮고, `topics = ["agent."]`를 적은 plugin은 같은 플래그 하나로 사용자가 친 것까지 받는다.
"대화를 포함한다"가 한쪽으로는 반만 참이고 다른 쪽으로는 너무 넓어진다.

### 위험 요소

- **취소가 turn 경계를 넘어 샌다 — 두 갈래다.** 첫째, `RunConfig.cancel`은 `Watchdog::start`가
  `cfg.cancel.child_token()`으로 소비하는데(`agent_loop.rs:762`) `CancellationToken`은 래치이고
  **이미 취소된 토큰의 자식은 취소된 채로 태어난다.** `drive`는 토큰을 하나만 만들어
  (`run.rs:273`) 신호 처리기·`Screen`·`cfg`에 나눠 준다. 그대로 두면 대화 중 **첫** Ctrl-C 이후의
  모든 turn이 모델을 부르기도 전에 `StopReason::Cancelled`로 끝난다. 둘째,
  `asked_to_cancel`은 `Screen::start`가 한 번 spawn하는 펌프 태스크의 지역 변수다
  (`run.rs:452-458`). 화면이 turn을 넘겨 살면 이 플래그도 `true`로 남아, turn 2의 **첫** Ctrl-C가
  `ForceExit`가 되어 프로세스를 죽인다 — Phase 1의 두 겹 보장이 한 겹이 된다. 그리고
  `a_second_cancel_intent_forces_the_exit`(`run.rs:602`)는 `Reaction::to`의 순수 단위 테스트라
  자기 지역 `asked`를 쓰므로 **이 회귀를 보지 못한 채 초록으로 남는다.**

  고칠 때 조심할 것: **"토큰을 turn마다 새로 만든다"만으로는 더 나빠진다.** 어려운 소비자는
  하나뿐인데, 하필 그 하나가 취소를 실행하는 쪽이다. 신호 처리기는 문제가 아니다 —
  `signals::install`(`run.rs:274`)과 그 `abort`(`run.rs:323`)가 **둘 다 `drive` 안에** 있으므로
  통째로 turn 루프 안으로 들어가면 매 turn 그 turn의 토큰을 집는다. 문제는 `Screen`의 펌프다:
  화면이 turn을 넘겨 사는 이상 한 번만 spawn되는데(`run.rs:452-458`), 시작할 때 받은 토큰을
  복제해 쥐고(`run.rs:453`) 거기서 취소한다(`run.rs:555`). `cfg`에만 새 토큰을 주면 펌프는
  **turn 1의 죽은 토큰**을 계속 취소하므로 turn 2부터 Ctrl-C가 **아무 일도 하지 않는다** —
  래치 버그보다 나쁘다(그쪽은 적어도 멈추기는 한다).

  그리고 "취소되지 않는 부모를 오래 두고 turn마다 자식을 낸다"는 답이 **아니다.** 취소하는
  것이 펌프이므로, 펌프가 부모를 쥐면 첫 Ctrl-C가 부모를 래치해 이후 모든 자식이 취소된 채
  태어난다 — 되돌아온 원래 버그다. 펌프가 지금 turn의 자식을 쥔다면 그것은 이미 "갈아 끼울 수
  있는 핸들"이다. 그러니 답은 하나다: **펌프가 보는 것을 turn마다 갈아 끼울 수 있게 한 겹
  간접을 두는 것.** 4b.6이 그 한 겹을 어디에 두는지 적어야 한다.
- **컴포저와 기존 키가 부딪힌다 — 그리고 목록은 `c`보다 길다.** `app.rs:243-247`은 맨 `c`를
  Cancel로 두면서 "취소하려고 그냥 `c`를 친 사용자도 취소를 의미했다"고 적었는데, 컴포저에서
  그것은 문자 `c`여야 한다. 그런데 **더 급한 것은 `q`다** — `app.rs:248`은
  `(KeyCode::Char('q') | KeyCode::Esc, _) => Some(Intent::Quit)`이므로, 컴포저에서 `q`를 치면
  대화가 끝난다. 한국어든 영어든 타이핑에 `q`가 `c`보다 흔하지는 않더라도, 결과가 "취소"가
  아니라 "종료"라 더 나쁘다. `Tab`도 있다 — 오늘은 패널 포커스를 넘긴다(`app.rs:249-256`).
  그러니 모드로 갈라야 하는 것은 넷이다: `c` · `q` · `Esc` · `Tab`. 그리고 승인 모달이 떠 있는 동안 부딪히는 키는 **정확히 넷**이다 — `y` · `n` ·
  `Esc`, 그리고 `allow_remember`일 때의 `a`. 그 밖의 키는 삼켜지지 않는다:
  `answer_approval`의 `match`가 `_ => return false`로 떨어뜨리고(`app.rs:217`) `handle_key`가
  그대로 intent 매핑으로 넘긴다. 그러니 고칠 것은 "모달이 타이핑을 먹는다"가 아니라 **그 넷**의
  뜻을 모드로 가르는 일이다. `Esc`는 모달이 없을 때만 `Quit`이고(`app.rs:248`) 모달이 있으면
  Deny인데, 컴포저에서의 셋째 뜻은 정해져 있지 않다. Ctrl-C만은 두 모드·모달 유무와 무관하게
  Cancel로 남는다.
- **프롬프트가 유실될 수 있는 채널을 탄다.** `mpsc::channel(8)`에 `try_send`이고
  (`app.rs:78`·`:263`), 그 선택의 근거는 "채널에 이미 읽히지 않은 intent가 있으니 중복은
  버려도 된다"였다. `Quit`·`Cancel`처럼 멱등한 것에는 맞고, 사용자가 방금 친 **고유한 내용**에는
  틀리다.
- **긴 대화는 멈추지 않는다. 조용히 잊는다 — 그게 더 나쁘다.** 대화가 길어지면 모델이
  거절할 것 같지만 아니다. `history::trim`(`context/history.rs:72-110`)이 **가장 오래된 turn
  그룹부터 버리고 계속 간다.** `LimitReached { ContextSize }`는 그 경로가 아니라 **도구 스키마만으로
  예산이 차는** 경우이고(`agent_loop.rs:389-397`, 주석이 "trimming history cannot recover it"이라
  적었다) 대화 길이와 무관하게 turn 1에 터지거나 영영 안 터진다. `ContextOverflow`도 가장
  **새** turn이나 시스템 프롬프트 혼자 안 들어갈 때뿐이다(`context/mod.rs:140-146`·`:162-176`).

  그래서 진짜 위험은 천장이 아니라 **조용함**이다. `assemble`은 무엇을 버렸는지 이미 세어
  `dropped`에 담고 그 doc이 "so a UI can warn about both"라고 적었는데(`context/mod.rs:221-230`),
  `AssembledContext.dropped`를 읽는 코드가 `agent_loop`·`rivet-cli`·`rivet-tui` 어디에도
  **없다.** 한 번 뜨고 마는 화면에서는 그럭저럭 넘어갔다. 대화형 화면에서는 사용자가 위로 스크롤해
  읽을 수 있는 transcript를 모델은 더 이상 갖고 있지 않게 되고, 아무도 그 사실을 말해 주지 않는다.
  `SessionEvent::Checkpoint`에 아직 **기록자가 없다는 것**(비-테스트 참조가
  `session.rs:377-395`의 `apply` 팔 하나뿐이다)이 이것을 고치지 못하는 이유이고, 열린 질문
  §11-1이 그 자리다. 이 Phase는 §11-1을 닫지 않되, **버려졌다는 사실을 화면에 올리는 것**은
  4b.5가 가져간다 — 이미 세어 둔 수를 그리는 일이라 압축 전략을 기다릴 필요가 없다.
- **루프 안에 남으면 안 되는 것이 셋 더 있다.** `watching.finish()`(`run.rs:337`)는 observer를
  **가져가서** 드레인하고, `Observer`의 `Drop`은 펌프를 abort한다(`run.rs:112`가 그렇게 적었다)
  — 루프 안에 남으면 화면이 turn 2부터 이벤트를 못 받는다. `AgentLoop`는 `run.rs:312-319`에서
  한 번 만들어지는데 `FullJitter::for_run(cfg.run_id)`가 **turn 1의 `run_id`로** 씨를 받으므로,
  루프 밖에 두려면 그 씨가 대화 단위라는 것을 받아들이는 것이다(받아들일 만하다 — 적어 두는
  것으로 족하다). 반대로 `RunConfig`는 `run()`에 **move**되므로(`run.rs:321`) 어차피 turn마다
  다시 만들어지고, `cfg.run_id`도 그때 새로 난다. 4b.6의 "turn마다"에는 수리·토큰·플래그
  말고 이것들이 어디에 서는지도 들어간다.
- **`AppState`에 turn을 넘겨 살면 안 되는 필드가 있다.** `RunStarted` 팔은 `run.stop`만 지운다
  (`state.rs:184-187`). `run.retries`와 `run.last_error`는 run 단위라, turn 1의 실패가 turn 5까지
  화면에 남는다. 그리고 `run.text`는 8 KB 링 버퍼이고(`TEXT_LIMIT`, `state.rs:24`;
  `push_text`, `state.rs:352-364`) `run.tools`는 200개에서 잘린다. 이것을 대화 transcript로
  바꾸는 것은 4b.5가 인정하는 것보다 큰 변경이며, 스크롤백은 아직 아무 항목도 없다.
- **`report()`가 대체 화면 안에 찍힌다.** 위험은 `Screen::drop`이 아니다 — 그것은
  `run.rs:491-509`에 **구현되어 있고** 두 태스크를 abort하므로 루프 안의 `?`도 터미널을 되돌린다.
  진짜 위험은 `stop`이 문서로 적어 둔 순서다(`run.rs:472-478`): `stop`은 렌더 태스크를
  **await**해서 `guard.restore()`가 출력보다 먼저 일어나게 하는데, `Drop`은 await할 수 없다.
  turn 루프에서는 `report(&summary)`의 호출 지점(`run.rs:363`)이 매 turn 지나는 자리에 있고, 그러면 요약이
  곧 사라질 대체 화면 위에 찍힌다.
- **transcript가 유실되는 버스를 타는데, 이번엔 되돌릴 길이 없다.** 결정 쪽에서 "사용자 turn은
  이벤트로 온다"고 정했지만, 버스는 설계상 lossy다. 이 저장소는 그 대가를 이미 한 번 치렀다 —
  `upsert_tool`의 doc이 그 이유를 적어 두었다(`state.rs:395-397`): "버스는 lossy하므로, 시작을
  본 줄만 갱신하는 UI는 `tool.requested`를 놓친 호출에 대해 **아무것도** 보여 주지 않게 된다."
  그래서 도구 줄에는 뒤늦게 만들어 주는 길이 있다. `agent.input.received`에는 그런 길이 없고,
  envelope 하나를 놓치면 사용자 turn 하나가 transcript에서 통째로 사라진다 —
  `the_transcript_shows_the_user_turn_from_events_alone`이 단언하려는 것과 정반대다. 다행히
  고칠 재료는 이미 손에 있다: turn 루프가 어차피 매 turn 로그를 다시 읽으므로(위의 결정),
  그 `SessionState`가 화면이 놓친 것을 메우는 권위 있는 사본이다. 4b.5가 그 보수 경로를
  갖거나, 갖지 않기로 하고 그 사실을 적어야 한다.

### DoD

- [ ] run이 끝나도 화면이 남고 다음 프롬프트를 받는다
      (`the_host_does_not_stop_the_screen_when_a_run_ends` — **호스트 수준**이어야 한다.
      "`agent.run.completed`를 fold해도 `is_dismissed()`가 거짓"은 오늘도 참이라 아무것도
      붙들지 못한다: fold의 `RunCompleted` 팔은 `run.stop`만 세우고(`state.rs:211-213`)
      화면을 죽이는 것은 `screen.stop()`(`run.rs:344-346`)이기 때문이다. 단언할 것은 run이
      끝난 **뒤에** 호스트가 그것을 부르지 않는다는 것이다 · `a_second_prompt_continues_the_same_session` — 두 turn이
      같은 `SessionId`를 쓰고 둘째 run의 `SessionState`가 첫 turn의 메시지를 담는다)
- [ ] turn마다 로그를 **수리하고 나서** 읽고, 끝까지 읽는다
      (`each_turn_repairs_the_log_before_it_reads_it` — 답 없는 tool call을 남긴 turn 뒤에
      다음 turn을 돌리고, 그러고도 `inspect`가 `Rli::Broken`이 아님을 단언한다. 수리를 빼면
      `rivet resume`이 영구히 거절하므로 이 테스트가 그 회귀를 잡는다 ·
      `a_conversation_past_one_read_page_keeps_its_middle` — 1,000 이벤트를 넘겨도 `last_seq`가
      로그의 끝과 같다)
- [ ] 취소가 turn 경계를 넘지 않는다 — 두 갈래이므로 두 테스트다
      (`a_cancelled_turn_does_not_cancel_the_next_one` — turn 1을 취소하고 turn 2가 모델까지
      도달함을 단언 · `a_later_turn_can_still_be_cancelled` — **turn 2의 Ctrl-C가 turn 2의 run을
      실제로 멈추는가.** 이 셋째가 없으면 "펌프가 죽은 토큰을 취소한다"는 회귀가 앞의 둘을
      모두 통과한다: turn 2는 모델에 도달하고(첫째 만족) `Reaction::to`는 순수 함수라
      `CancelTheRun`을 돌려준다(둘째 만족) · `the_two_step_cancel_resets_each_turn` — turn 2의
      첫 Ctrl-C가 `CancelTheRun`이지 `ForceExit`가 아니다. 기존 `a_second_cancel_intent_forces_the_exit`는
      `Reaction::to`의 단위 테스트라 이 회귀를 못 보므로, **펌프 수준의** 테스트여야 한다)
- [ ] 컴포저가 키를 삼키지 않는다 — 세 방향이므로 세 테스트다
      (`ctrl_c_still_cancels_while_composing` ·
      `a_typed_key_is_text_rather_than_a_command_while_composing`(맨 `c`가 문자 `c`가 된다) ·
      `an_approval_modal_does_not_eat_the_composer`. 기존
      `ctrl_c_in_the_tui_asks_for_a_cancel`은 그대로 통과해야 한다)
- [ ] 제출한 프롬프트는 버려지지 않는다
      (`a_submitted_prompt_is_never_dropped` — 채널이 꽉 찬 상태에서 제출해도 호스트가 그것을
      받는다. `try_send`의 "중복은 버려도 된다"는 근거가 고유한 내용에는 성립하지 않는다)
- [ ] `q`가 그리기가 아니라 대화를 끝낸다
      (`quitting_ends_the_conversation_rather_than_only_the_drawing` — dismiss 뒤 turn 루프가
      입력을 더 묻지 않는다. Phase 4의 `a_dismissed_screen_is_not_a_sink`는 그대로 성립한다)
- [ ] 사용자 turn이 이벤트만으로 그려지고, run 단위 필드는 turn 경계에서 정리된다
      (`the_transcript_shows_the_user_turn_from_events_alone` — 손으로 만든 envelope만으로.
      `every_panel_is_filled_from_events_alone`의 확장 ·
      `a_new_turn_clears_what_belonged_to_the_last_one` — `retries`·`last_error`)
- [ ] 새 토픽이 좁은 프로파일에 닿지 않고, `include_conversation`이 **양쪽**을 덮는다
      (`the_prompt_echo_is_withheld_from_a_narrowed_profile` — Phase 3의
      `a_readonly_profile_keeps_the_conversation_from_a_subscriber`와 짝 ·
      `an_input_prefix_also_needs_include_conversation` — **새 절반**을 겨냥해야 한다.
      `topics = ["agent."]`는 기존 테스트가 이미 도는 세 모양 중 하나라
      (`telemetry-log/src/lib.rs:496`이 `["agent.text.", "agent.", "agent.text.delta"]`를
      순회한다) 고치기 전에도 통과한다. 새 테스트는 `topics = ["agent.input."]`가 플래그
      없이 거절되는 것과, `include_conversation = true`가 접두사 **둘 다**를 밀어 넣는 것
      (`lib.rs:193-198`)을 봐야 한다. 기존
      `a_written_prefix_that_reaches_the_conversation_needs_include_conversation`
      (`telemetry-log/src/lib.rs:491`)은 그대로 통과해야 한다)
- [ ] 산문 열다섯 곳이 전부 손봐졌다
      (아래 [4b.8 표](#4b8--이-phase가-끝나는-날-거짓이-되는-문장)가 그 목록이고, 컴파일러도
      테스트도 한 곳을 막아 주지 않으므로 체크박스로 세운다)
- [ ] `answer()` 재출력이 사라지고, 요약이 대체 화면 위에 찍히지 않는다
      (`run.rs:388`의 함수와 "the first N character(s) scrolled out of the panel" 사과가 함께
      삭제된다 · e2e `a_conversation_does_not_reprint_its_own_transcript` ·
      `the_summary_is_printed_after_the_terminal_comes_back`. **이 삭제가 이 Phase가 필요했다는
      증거다** — 화면이 너무 일찍 죽는 것을 메우던 처치였다. 단, 사과를 그냥 지우면 8 KB를 넘긴
      대화가 아무 안내 없이 앞부분을 잃으므로, 링 버퍼를 바꾸거나 안내를 패널 안으로 옮기는
      것이 이 항목에 함께 들어간다)

### 4b.8 — 이 Phase가 끝나는 날 거짓이 되는 문장

Phase 4가 `design/phase-4-policy-sandbox.md` §3.9(문서·예제)에서 한 것과 같은 표다. 아래 문장들은 **오늘 전부 참이고**, 이 Phase가 끝나는 날 손봐야 한다. 미리 고쳐 둘 수 없는
이유가 그것이다 — 지금 고치면 구현될 때까지 문서 여섯과 예제 하나가 거짓말을 한다. 대신
어디를 고칠지를 여기 적어, 구현하는 사람이 다시 찾아다니지 않게 한다.

**두 종류가 섞여 있고 구분해서 읽어야 한다.** 대부분은 그날 **거짓이 된다**. 몇몇은 규칙
자체는 그대로인 채 **예시가 낡는다** — `config.md:207-208`·`:213-214`가 그렇다. 후자를
"틀렸다"고 읽으면 규칙을 고치려 들게 되므로 표의 "4b 이후" 칸에 어느 쪽인지 적었다.

원인은 **넷**이고 항목마다 다르다: 4b.3이 토픽을 하나 더하는 것, 4b.4가 `CONVERSATION_TOPIC`을
둘로 만드는 것, 4b.2·4b.6이 화면을 대화형으로 만드는 것, 그리고 한 번의 실행이 run 하나가
아니게 되는 것.

| 문장 | 지금 | 4b 이후 |
|---|---|---|
| `events.md:131-138` — agent 토픽 여덟 줄 | 여덟 | 아홉. `agent.input.received text`. **`← 저장 안 됨`은 붙지 않는다.** 그 표시는 "durable 쌍둥이가 없다"가 아니라 **"조각이라 저장되지 않는다"**는 뜻이다(그 표시를 단 둘, `agent.text.delta`와 `tool.execute.progress`가 그렇다). 사용자 입력은 조각이 아니라 통째이므로 표시 없이 들어간다 |
| `events.md:178` — "28개 중 23개는 … 5개는 아직 없다" | 28 / 23 / 5 | **29 / 24 / 5**. 새 토픽은 발행되는 쪽이다 |
| `security.md:398` — "`agent.text.delta`를 **한 번도** 받지 못한다" | 하나 | 둘. 좁은 프로파일은 사용자가 친 것도 받지 못한다 |
| `security.md:401` — "좁은 셋은 `agent.text`를 뺀 나머지" | 하나를 뺀 | 둘을 뺀. **이 문장이 예고한 경우가 바로 이것이다** — "`agent.` 밑에 토픽이 새로 생기면 누가 목록에 추가하기 전까지는 주어지지 않는다" |
| `architecture.md:756-758` — 같은 문장의 쌍둥이 | 하나 | 둘. 근거("대화 전문을 받을 이유가 없다")는 그대로이고 대상만 늘어난다. 그 근거는 `production` 하나가 아니라 좁은 셋 **모두**에 대한 것이다 |
| `plugin.md:154-155` — "`readonly`·`reviewer`·`production`은 `agent.text`를 주지 않는다" | 하나 | 둘. 도구 저자가 읽는 줄이므로 빠뜨리면 매니페스트를 잘못 쓴다 |
| `config.md:185`·`:187`·`:192-195`·`:204-205` — `telemetry-log`의 `include_conversation` 절 | `agent.text.` 하나를 가리킨다 | 4b.4가 `CONVERSATION_TOPIC`을 둘로 만들면 이 절 전체가 따라 움직인다. `:204`·`:205`의 표 두 행이 특히 그렇다 — "무엇이 로드 실패인가"가 바뀐다 |
| `config.md:207-208` — "좁은 프로파일 + `agent.text.`는 '권한 0'이 **아니라** '그것만 사라짐'" | 하나가 사라진다 | 둘이 사라진다. 규칙은 그대로이고 예시의 수가 바뀐다 |
| `config.md:213-214` — 워크드 예제 "`agent.request.` · `agent.run.` · `agent.turn.`만 남고 `agent.text.`가 사라지므로" | 남는 셋 / 사라지는 하나 | 남는 셋은 그대로, 사라지는 것이 **둘**. 결론(로드 실패)은 안 바뀐다 |
| `rivet.example.toml:92-93`·`:97-100` — `telemetry-log` 주석 두 덩이 | "기본은 `agent.text.`를 일부러 뺀다" · "대화 전문을 로그에 넣는다" | 뺀 것이 **둘**이 된다. `:94`의 `topics` 예시 줄 자체는 안 움직인다 — `DEFAULT_TOPICS`가 이미 대화를 빼고 있기 때문이다(`telemetry-log/src/lib.rs:63-70`). **Phase 4의 선례(§3.9)가 "문서·예제"인 이유가 이것이다** |
| `config.md:77-78` — "각 축은 … 프로세스는 종료 코드 3으로 끝난다" | 실행 하나 = run 하나이므로 참 | turn이 여럿이면 turn 1의 한계가 프로세스를 끝내지 않는다. 무엇이 이기는지는 4b.6이 정한다(아래 "닫지 않는 것") |
| `config.md:398` — `--tui` 행 | "전체 화면 UI (Job 패널 · Agent 패널 · 상태바)" | 대화형이라는 것과, `q`가 대화를 끝낸다는 것이 들어간다 |
| `plugin.md:174-175` — "부분적으로 좁혀지는 경우는 거절이 아니다" + `["tool.", "agent.text."]` 예제 | `config.md:207-208`과 **같은 규칙·같은 예제**인데 도구 저자가 읽는 쪽이다 | 예시가 낡는다. `plugin.md:154-155` 행과 같은 이유로 빠뜨리면 안 된다 |
| `README.md:19` — "다음은 Phase 4b(대화형 TUI)" | 참 | 이 Phase가 끝나는 날 거짓이 된다. **이번 diff가 방금 고친 줄이라 특히 잊기 쉽다** |
| `README.md`의 `rivet --tui "..."` 주석 | 같은 세 패널 | 같이 움직인다 |

이 표 자체가 DoD의 일부다: 위 **열다섯** 곳 중 하나라도 남으면 4b.8은 끝나지 않은 것이다.

**열다섯 곳 전부가 아무 테스트도 붙들지 않는 산문이다.** `every_bus_topic_is_claimed`가
`events.md:178`의 수까지 지켜 줄 것 같지만 아니다 — 그 테스트가 강제하는 것은
`event_flow.rs:133-157`·`:162-168`의 **코드 목록**이고, 산문에 적힌 "28개 중 23개"는 아무도
붙들지 않는다. 컴파일러가 한 곳도 막아 주지 않으므로 표로 적는다.

### 이 Phase가 닫지 않는 것

**§11-1(컨텍스트 압축).** 위험 요소에 적은 대로 checkpoint에는 아직 기록자가 없다. 이 Phase는
`dropped`를 **화면에 올릴** 뿐(4b.5), 무엇을 버릴지 고르는 전략은 건드리지 않는다 — 압축은
그 자체로 한 항목이고, 여기 끼워 넣으면 둘 다 반만 된다. 다만 이 Phase 이후로는 그 질문이
"언젠가 정할 것"이 아니라 **사용자가 화면에서 매일 보는 것**이 된다.

**`rivet resume --tui`.** `resume()`(`run.rs:176-208`)도 `resumed`를 거쳐 같은 `drive`에 닿으므로 —
그리고 그 앞에서 `read_all`·`close_interrupted`를 이미 지난다(`run.rs:187-194`) — 이 Phase가
끝나면 그것도 대화형이 된다. 의도한 결과이지만 `resume_plan` 게이트는 한 번만 지나가므로,
그 조합이 무엇을 뜻하는지는 4b.6이 정하고 `config.md`가 적는다.

**여러 turn의 종료 코드.** `finish`(`main.rs:330-335`)가 `RunSummary` 하나를 `exit::for_stop`(`:332`)으로 접는다
(`exit.rs:23-37`). N turn은 N개의 요약을 낸다. 마지막 것이 이긴다는 규칙이 가장 그럴듯하나,
정하는 것은 4b.6이고 여기서 미리 못 박지 않는다.

**`max_duration_ms`가 turn 단위가 된다.** `Watchdog`은 `run()`마다 시작하므로
(`agent_loop.rs:215`) 대화 전체가 아니라 turn마다 예산을 받는다. 그 편이 옳아 보이지만,
Phase 4의 PR 리뷰 blocking이 바로 이 숫자 위에서 벌어졌으므로 기록해 둔다.

---

## Phase 5 — Job Runtime

**목표**: 첫 번째 Demo가 동작한다.

```text
"로그인 API를 구현해줘"
  → Job 생성 → 저장소 조사 → 구현 → 테스트 → 실패 → 수정
  → 테스트 → 리뷰 → 승인 → 완료
```

| # | 작업 | crate |
|---|---|---|
| 5.1 | Job 저장 + 상태 전이 적용기 | `rivet-job` |
| 5.2 | Local scheduler (claim / release) | `rivet-job` |
| 5.3 | `Sequential` `Parallel` `ReviewGate` workflow | `plugins/workflow-default` |
| 5.4 | Reviewer agent (다른 모델·다른 도구 집합) | `rivet-job` |
| 5.5 | Job ↔ Run ↔ Session 연결 | `rivet-job` |
| 5.6 | TUI Job 패널 (진행 체크리스트) | `rivet-tui` |
| 5.7 | `rivet job list/show/cancel/review` | `rivet-cli` |
| 5.8 | 워크스페이스 동시성 결정 (열린 질문 §11-5) | — |
| 5.9 | 리뷰 verdict를 durable하게 기록 | `rivet-job` |

### DoD

- [ ] Demo 시나리오가 사람 개입 없이 끝까지 진행
- [ ] 리뷰 반려 → `READY` → 재시도 경로 동작
- [ ] `max_attempts` 소진 시 `FAILED`
- [ ] 실패한 의존성을 가진 Job이 `blocked()`로 보고됨
- [ ] Run이 죽어도 Job 상태가 살아남고 재개 가능

---

## Phase 6 — External Plugin

| # | 작업 |
|---|---|
| 6.1 | Process plugin 호스트 (spawn · 감독 · 재시작) |
| 6.2 | JSON-RPC 프로토콜 고정 + 버전 협상 |
| 6.3 | 능력 협상 (`CapabilityVersion` 핸드셰이크) |
| 6.4 | OS 수준 권한 분리 |
| 6.5 | 프로토콜 적합성 테스트 스위트 |

**DoD**: 같은 plugin 소스가 in-process와 process 모드에서 **동일하게** 동작.

---

## Phase 7 — Distributed (탐색)

Job queue · worker · remote agent · remote sandbox · persistent scheduler.
Phase 5의 `Scheduler` 계약이 이미 `claim`/`release`를 갖고 있으므로 계약 변경 없이
분산 구현을 끼울 수 있어야 한다. 그렇지 않다면 Phase 5의 계약이 틀린 것이다.

---

## MVP 경계

| MVP에 포함 | MVP 이후 |
|---|---|
| Workspace · Agent Loop · Session · Event Bus | Job Graph · Review Agent · Scheduler |
| Model 계약 + OpenAI 호환 어댑터 | Memory · Evaluation |
| Tool 계약 + filesystem · shell · git | Process Plugin · WASM Plugin |
| Plugin Registry · Policy · Local Sandbox | Distributed Runtime |
| CLI · 기본 TUI | Telemetry (otel · prometheus) |

**Job Runtime은 Rivet의 핵심 차별화 요소**이므로 MVP+ 단계에서 곧바로 추가한다.
Phase 5를 뒤로 미루면 Rivet은 "또 하나의 코딩 에이전트"가 된다.

---

## 마일스톤별 검증 명령

```bash
# 모든 Phase 공통 게이트
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps

# Phase 1
rivet "explain this repo"                  # 왕복 대화
rivet resume <session>                     # replay 재개

# Phase 2
rivet plugin list                          # 이 빌드의 plugin 목록 + 어느 것이 켜져 있는지
                                           # (구성만 본다. 등록 목록은 `rivet doctor`)
rivet plugin show rivet.tool-filesystem    # 권한 교집합 표시
rivet plugin new acme.tool-lint            # 새 plugin crate 스캐폴딩

# Phase 4  -- 둘 다 테스트로 대체되어 있다 (실제 provider 없이 성립함을 보인다):
#   readonly_offers_no_write_tool_and_the_model_is_told
#   headless_refuses_a_destructive_command_without_hanging
rivet --profile readonly "delete all logs" # 거부되어야 함
rivet --headless "rm -rf /"                # 매달리지 않고 거부

# Phase 4b  -- raw mode 경로는 자동화 테스트가 없다 (Phase 3이 남긴 것, §"미검증으로 남긴 것").
#              화면이 run 뒤에도 남는지는 손으로 본다.
rivet --tui "explain this repo"            # 답이 끝나도 화면이 남고, 이어서 다음 질문
                                           # q 로 대화를 끝낸다 (Ctrl-C 는 여전히 취소)

# Phase 5
rivet run --job "implement login API"     # Demo
rivet job list
```

---

## 진행 추적

| Phase | 상태 | 게이트 |
|---|---|---|
| 0 Repository | ✅ 완료 | 135 passed · clippy 0 · 리뷰 2회전 반영 완료 |
| 1 Minimal Agent | ✅ 완료 | 385 passed (+250) · clippy 0 · `cargo doc` 0 · DoD 8개 전부 충족 (1번은 실제 provider 수동 검증) · build 리뷰 지적 11건 반영 |
| 2 Plugin | ✅ 완료 | 456 passed (+71) · clippy 0 · `cargo doc` 0 · DoD 5개 전부 충족 · 롤백 무결성(`Err`·패닉·`load` 이후의 뒤늦은 등록) 테스트로 확인 · 설계 [`design/phase-2-plugin-loader.md`](./design/phase-2-plugin-loader.md) · 리뷰 2라운드 지적 반영(등록 창구 봉인 · 배치 승격 범위 · `plugin show`의 ABI 거부 표시) · 리뷰 3라운드 반영(성공 경로 봉인 테스트 · ABI 판정 단일 출처 · `seal` 비공개화) |
| 3 Event | ✅ 완료 | 559 passed (+92) · clippy 0 · `cargo doc` 0 · DoD 6개 전부 충족 · `rivet-tui`에서 `rivet-runtime` 의존 제거(컴파일 성질) · `events_subscribe` scope 강제(`architecture.md` §11-10 닫힘) · 새 plugin `rivet.telemetry-log`(기본 선택 밖) · 설계 [`design/phase-3-event.md`](./design/phase-3-event.md) · 리뷰 2라운드 지적 반영 · 구현 중 발견해 고친 것: 랙 보고 되먹임 폭주(40 → 118,312) |
| 4 Policy/Sandbox | ✅ 완료 | 772 passed (+193, base 579 — 표 아래 각주) · clippy 0 · `cargo doc` 0 · DoD 6개 전부 충족 · 심볼릭 링크 탈출 차단(`cwd`와 git 경로 인자 양쪽, `a_symlinked_cwd_pointing_outside_the_workspace_is_refused` · `a_git_path_argument_cannot_leave_the_workspace_through_a_link`) · 새 plugin 넷(`policy-default` `sandbox-local` `tool-shell` `tool-git`, 전부 기본 선택) · `ProcessSpawn`이 처음으로 주어짐(`architecture.md` §11-10 닫힘) · §11-7(비-UTF-8) · §11-14(spec 등록 시점 고정) 닫힘 · §11-9(도구 egress)는 의도적으로 다시 열어 **물려받음** · 설계 [`design/phase-4-policy-sandbox.md`](./design/phase-4-policy-sandbox.md) · 리뷰 4라운드: 1–3라운드 blocking은 전부 설계 단계에서 닫혔고(미등록 provider를 7단계·`doctor` 양쪽에서 거부로 만들던 것 · DoD 6을 증명할 정책이 체인에 없던 것 · 기존 단언 둘을 깨뜨리던 것), 4라운드 non-blocking 넷을 빌드에서 반영(`plugin.md`의 `process_spawn` 블록 · 관계 단언을 `BTreeSet` 비교로 · `every_capability_slot_is_readable`에 sandbox 슬롯 · 동시성 테스트 여유 1.67배 → 2배) · 구현 중 발견해 고친 것: 상속된 파이프를 쥔 손자가 끝난 호출을 무한정 붙잡는 것, 탈출하는 심볼릭 링크가 `git` 경로 인자에서 부모 검사로 새는 것 · **빌드 리뷰 1라운드 blocking: argv 주입 취약점** — `git_diff`의 `rev`가 검증 없이 argv로 들어가 `rev = "--output=…"`이 워크스페이스 밖에 파일을 쓴다(`read_only`를 신고한 도구가, `fs_write`를 주지 않는 `readonly`·`reviewer`에서). 호출 지점마다 검사를 더하는 대신 규칙을 타입으로 옮겼다 — `rivet_runtime::argv::Argv`(신규, `push` 없음, 문 넷: `&'static str` flag · 옵션에 묶인 값 · 옵션으로 읽힐 수 있으면 거부하는 operand · `--` 뒤의 pathspec)를 `tool-git`·`tool-shell`의 **모든** argv 항목이 지난다. 붙드는 테스트도 인자 이름을 나열하지 않는다: `no_declared_argument_can_become_an_option`이 스키마를 훑어 선언된 모든 문자열 인자에 옵션 모양의 값을 먹이고, e2e `a_readonly_run_cannot_write_through_a_git_argument`가 실제 바이너리로 파일이 없음을 단언한다 · 빌드 리뷰 1라운드 non-blocking 여섯도 반영(C1 회귀 테스트 · `settle`의 리더 `abort` · `tool.called` append를 scope 위로 올려 teardown을 함수의 모양으로 보장 · 승인 쌍의 requested 단언 · 패닉 경로 teardown 테스트 · `tool.policy.evaluated` 커버리지) · 빌드 리뷰 2라운드 PASS(blocking 없음) · **PR 리뷰 1라운드 blocking: 승인 sink가 화면보다 오래 산다** — `--tui`에서 `q`는 렌더 루프만 끝내고 런은 계속하게 두는데(설계대로다) `cfg.approval_sink`는 같은 `Arc<Tui>`를 계속 쥐고 있었다. 키 경로가 사라졌으므로 그 뒤에 승인이 필요한 호출은 `max_duration_ms`(기본 30분) 동안 아무것도 그리지 않은 채 멈춘다 — `--headless`와 `render/approve.rs`가 각각 막던 실패가 셋째 문으로 들어온 것이고, 둘 다 **런 시작 시점**만 본다. "화면이 없다"를 sink가 아는 상태로 만들어 닫았다: `Tui::dismiss()`가 그리기 종료와 sink 폐쇄를 한 사실로 묶고, 렌더 루프는 자기가 끝나는 모든 경로에서(터미널이 죽는 `?` 포함) 그것을 부르며, `Approvals::decide`가 이미 sink의 `Err`를 `Denied`로 접으므로 닫히는 방향이다 · 같은 라운드의 non-blocking 여덟도 반영: `Argv::option`의 플래그를 닫힌 타입 `Consuming`으로 바꿔 `option_exposure`의 binder 규칙이 추측이 아니라 그 목록이 되게 함(일반 순회가 `option("--staged", 값)` 오용을 이제 본다) · 순회가 `type == "string"`이 아니라 "문자열을 담을 수 없는 타입"만 제외하고 선언된 **전체** 이름 집합을 단언 · `Argv::pathspec`이 빈 pathspec을 버려 `path: "."`의 exit 128 루프를 끊음 · 도구가 자기 안에서 올린 거부의 감사 라벨을 `workspace`(평가한 적 없는 정책) → `tool`로 고치고 호스트가 쓰는 이름 셋을 전부 예약 · TUI의 두 뮤텍스를 한 순서로 함께 잡음 · `SandboxScope`가 `prepare`를 락 밖에서 하도록 `Preparing` 상태 도입(teardown이 준비 중인 provider를 기다리지 않고, 그 사이 봉인되면 갓 만든 핸들을 스스로 해제) · `require_approval_for_all_in`의 `allow_remember: true`는 유지하되 이유를 코드에 적고, 셸 형태 규칙을 그 앞으로 옮겨 "셸 게이트는 기억되지 않는다"가 프로파일 설정으로 뒤집히지 않게 함 · 리뷰어가 ubuntu CI(run `34199445217`, head `f5933b8`)가 이미 초록임을 확인 — §6.4의 "CI(ubuntu)와 darwin 양쪽" 요구는 충족돼 있다 |
| 4b Conversation | ⬜ | 화면이 run보다 오래 산다 |
| 5 Job Runtime | ⬜ | Demo 무개입 완주 |
| 6 External Plugin | ⬜ | 동일 소스 양쪽 동작 |
| 7 Distributed | ⬜ | 계약 변경 없이 분산 구현 |

> **각 행의 `(+N)`은 그 Phase의 base 커밋 대비 증가분이지, 윗줄의 수에서 이어지는 것이 아니다.**
> 두 수가 갈라질 수 있다: 어떤 행의 수는 그 Phase가 **끝난 시점**의 수이고, 다음 Phase는 그
> 뒤에 얹힌 커밋들 위에서 시작하기 때문이다. Phase 4가 그 경우다 — Phase 3 행의 559는 Phase 3
> 종료 시점의 수이고, Phase 4의 base인 `4c7bfae`에서 실측하면 **579**다. 그래서 559 + 181이
> 아니라 579 + 181 = 760이다. 행마다 base를 적는 것은 그 차이가 실제로 생겼을 때뿐이다.
