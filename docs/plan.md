# Rivet 작업 계획

> 상태: Draft v0.3 (설계 리뷰 반영) · 갱신: Phase 0 완료 시점
> 설계 근거는 [`architecture.md`](./architecture.md)에 있다. 이 문서는 **무엇을 어떤
> 순서로 만들고, 무엇으로 완료를 판정하는가**만 다룬다.

---

## 0. 우선순위 원칙

가장 먼저 만들 것은 Plugin Marketplace가 아니다. 순서가 중요하다.

```text
Agent Loop → Session Event → Tool 계약 → Model 계약 → Event Bus
  → Plugin Registry → Policy → Sandbox → Job Runtime → TUI → External Plugin
```

특히 **`Agent Loop + Session + Event`를 먼저 안정화해야** 나머지를 안전하게 확장할 수
있다. 이 셋이 흔들리는 상태에서 Job Runtime을 얹으면 두 배로 고쳐야 한다.

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

- [ ] 실제 provider로 왕복 대화가 동작 — **미검증**[^live]
- [x] tool call → 파일 읽기 → 모델이 결과를 사용
      (`rivet-cli/tests/e2e.rs::a_prompt_runs_a_tool_and_the_model_sees_its_result`:
      두 번째 요청의 메시지 배열에 파일 내용이 담긴 tool result가 있음을 단언)
- [x] Ctrl-C가 5초 안에 in-flight 요청과 도구를 정지 — 오프라인 검증분만[^ctrlc]
- [x] `rivet resume <session>`이 로그 replay로 이어감
      (`rivet-runtime/tests/session_recovery.rs`, `rivet-cli/tests/e2e.rs`)
- [x] 5개 한도가 각각 발동하는 테스트 존재
      (`rivet-runtime/tests/agent_loop.rs`의 `the_*_limit_trips` 5개, 각각 어느
      `LimitKind`인지까지 단언)
- [x] provider 응답 픽스처 기반 offline 테스트 통과
      (`plugins/model-openai/tests/fixtures/*.sse` 9개 + 디코드·인코딩·에러 분류·루프백)
- [x] 심볼릭 링크 탈출 시도가 차단됨 (1.6b)
      (`rivet-runtime/tests/fsguard.rs`, unix 한정 — windows는 미검증)
- [x] `kill -9` 후 재개 시 미완 tool call이 합성 결과로 닫힘
      (`rivet-cli/tests/e2e.rs::a_killed_run_is_closed_with_a_synthetic_result_and_resumes`
      — 실제 SIGKILL)

[^live]: 이 환경에 API 키가 없어 실제 왕복을 **수행하지 못했다.** 충족했다고 표시하지
    않는다. 오프라인 대체물은 (a) 실제 `reqwest` 경로를 지나는 루프백 SSE 서버 테스트,
    (b) 9개 픽스처 디코드 테스트, (c) 서브프로세스 e2e다. 수동 검증 절차는
    `HANDOFF.md`에 있다.

[^ctrlc]: 취소 메커니즘 자체(스트림 drop → 서버가 연결 종료를 관측, 협조 취소, 유예 초과
    포기, 취소된 로그의 재개 가능성)는 오프라인으로 검증했다. **터미널에서 사람이 누르는
    실제 Ctrl-C는 수동 검증 대기**이며 `HANDOFF.md`에 있다.

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

- [ ] 부분 등록 후 실패한 plugin이 **아무것도** 남기지 않음 (테스트)
- [ ] ABI 불일치 plugin이 등록 시도 전에 거부됨
- [ ] `readonly` 프로파일이 쓰기 plugin의 쓰기 권한을 실제로 제거
- [ ] 이름 충돌 시 양쪽 plugin 이름이 에러에 나옴
- [ ] unload 후 같은 이름 재등록 성공 (hot reload 전제)

---

## Phase 3 — Event

**목표**: plugin이 루프를 수정하지 않고 실행을 관찰한다.

| # | 작업 | crate |
|---|---|---|
| 3.1 | 전체 이벤트를 실제 발행 지점에 연결 | `rivet-runtime` |
| 3.2 | `EventSubscriber` 등록 + 토픽 필터 | `rivet-runtime` |
| 3.3 | `telemetry.log` plugin (구조화 로그) | `plugins/` |
| 3.4 | JSONL 이벤트 스트림 (`--jsonl`) | `rivet-cli` |
| 3.5 | 기본 TUI (Job 패널 · Agent 패널 · 상태바) | `rivet-tui` |

### DoD

- [ ] TUI가 런타임 타입을 **하나도** import 하지 않고 이벤트만 소비
- [ ] 느린 구독자가 루프를 지연시키지 않음 (측정)
- [ ] `SubscriberLagged`가 실제로 발행됨
- [ ] 등록된 subscriber plugin이 실제로 이벤트를 받음 (`attach_subscriber`)
- [ ] 언로드된 plugin의 구독 태스크가 중단됨
- [ ] `--jsonl`이 관찰 가능성을 제공 — **세션 재구성용이 아님**

> 원래 DoD는 "`--jsonl` 출력만으로 세션 재구성 가능"이었다. 이것은 버스가 lossy라는
> 설계와 모순이다. 유실될 수 있는 스트림으로 durable 로그를 재구성할 수는 없다.
> 세션 재구성이 필요하면 세션 로그를 export 해야 한다 (`rivet session show --json`).

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

- [ ] `--headless`에서 승인 요구가 매달리지 않고 거부됨
- [ ] 승인/거부가 세션 로그에 durable event로 남음
- [ ] resume 후에도 "세션 동안 기억" 승인이 유지됨
- [ ] 취소 시 프로세스 트리 전체 종료 (좀비 없음)
- [ ] 거부된 도구가 세션에 `ToolBlocked`로 남고 모델이 사유를 봄
- [ ] `readonly` 프로파일에서 `write_file`이 실제로 거부됨

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
rivet plugin list                          # 등록 목록
rivet plugin show rivet.tool-git           # 권한 교집합 표시

# Phase 4
rivet --profile readonly "delete all logs" # 거부되어야 함
rivet --headless "rm -rf /"                # 매달리지 않고 거부

# Phase 5
rivet run --job "implement login API"     # Demo
rivet job list
```

---

## 진행 추적

| Phase | 상태 | 게이트 |
|---|---|---|
| 0 Repository | ✅ 완료 | 135 passed · clippy 0 · 리뷰 2회전 반영 완료 |
| 1 Minimal Agent | ✅ 완료 (1건 수동 검증 대기) | 379 passed (+244) · clippy 0 · `cargo doc` 0 · DoD 8개 중 7개 충족, 1번(실제 provider 왕복)은 API 키 부재로 미검증 |
| 2 Plugin | ⬜ | 롤백 무결성 |
| 3 Event | ⬜ | TUI가 런타임 타입 미참조 |
| 4 Policy/Sandbox | ⬜ | 심볼릭 링크 탈출 차단 |
| 5 Job Runtime | ⬜ | Demo 무개입 완주 |
| 6 External Plugin | ⬜ | 동일 소스 양쪽 동작 |
| 7 Distributed | ⬜ | 계약 변경 없이 분산 구현 |
