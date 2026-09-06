# Rivet

**The model thinks. The runtime acts. Plugins define behavior.**

Rivet is a programmable AI agent runtime written in Rust. It is not a coding agent — it is
the runtime a coding agent (or a review agent, or a CI agent) is built *on*.

The bet: the durable value is not in any one tool, it is in a **stable set of capability
contracts** that plugins extend.

```text
Plugin · Model · Tool · ContextProvider · Policy · Sandbox
Session · Agent · Workflow · Task · Scheduler · Evaluator
```

> 상태: Phase 1 완료 — **실제로 도는 에이전트**. 모델과 왕복하고, 파일을 읽고 쓰고,
> 세션을 append-only 로그에 fsync하며, Ctrl-C와 `kill -9` 뒤에도 재개된다.
> 다음은 Phase 2(plugin 로더). 설계 문서는 [`docs/`](./docs)에 있다.

---

## 왜 또 하나의 에이전트 런타임인가

대부분의 코딩 에이전트는 하나의 통합된 프로그램이다. Shell 실행, Git 조작, 권한 판단,
세션 저장, UI가 한 루프에 얽혀 있다. 처음 3개월은 빠르고, 그 뒤로는 모델 교체 · 정책
변경 · 감사 · 재개 · 확장이 전부 어렵다.

Rivet은 그 루프를 작게 유지하고, 나머지를 전부 교체 가능한 계약으로 만든다.

| | 통합형 에이전트 | Rivet |
|---|---|---|
| 도구 추가 | 루프 수정 | plugin 등록 |
| 권한 규칙 | 각 도구가 자체 판단 | 독립 `Policy` capability |
| 세션 | 로그 파일 | event-sourced, replay·fork 가능 |
| 장기 작업 | 없음 | `Task` 그래프 + 리뷰 게이트 |
| UI | 루프에 결합 | 이벤트 스트림 소비자 |
| 격리 | 있거나 없거나 | 보장을 스스로 신고하는 `Sandbox` |

---

## 지금 되는 것

```bash
export DEEPSEEK_API_KEY=...                       # rivet.toml의 api_key_env가 가리키는 변수
rivet "list the files here and explain what this project does"
rivet resume ses_...                              # 중단된 세션을 이어서
rivet session list | rivet session show <id> --json
rivet doctor                                      # 설정·deny list·자격증명·plugin 점검
```

`rivet.example.toml`을 `rivet.toml`로 복사하면 그대로 동작한다. 아직 없는 plugin
(`tool-shell` `tool-git` `policy-default` `sandbox-local`)은 경고 후 건너뛴다.

두 가지는 이름과 달리 오해하기 쉬우므로 분명히 해 둔다.

- **`--jsonl`은 관찰용이다.** 이벤트 버스는 설계상 lossy이므로(느린 구독자는 이벤트를
  잃고, 그 사실은 `SubscriberLagged`로 보고된다) 이 스트림으로 세션을 재구성할 수 없다.
  세션 재구성은 durable 로그를 읽는 `rivet session show --json`이다.
- **Phase 1의 `--profile`은 policy가 아니다.** 프로파일은 에이전트에게 어떤 도구를
  **제공할지**를 좁힌다(파이프라인 2단계). `--profile readonly`는 `write_file`을 아예
  등록하지 않으므로 모델이 부를 수 없지만, 이것은 정책 강제가 아니다. 진짜 policy chain과
  승인은 Phase 4다.

---

## 구조

```text
crates/
  rivet-core       계약만. I/O 없음. 모든 plugin이 이것에 컴파일된다
  rivet-runtime    기본 실행 구현 (loop · dispatcher · bus · registry)
  rivet-session    append-only 이벤트 로그
  rivet-task       Task 그래프 실행
  rivet-plugin     탐색 · 매니페스트 · lifecycle
  rivet-tui        TUI (이벤트 스트림 소비자)
  rivet-cli        `rivet` 바이너리

plugins/
  model-openai     OpenAI 호환 (OpenAI · DeepSeek · Ollama · vLLM · OpenRouter)
  tool-filesystem  read · write · list · search
  tool-shell       샌드박스 경유 셸
  tool-git         status · diff · log · commit
  policy-default   워크스페이스 봉쇄 · 파괴적 명령 게이트
  sandbox-local    로컬 실행 (약한 보장을 정직하게 신고)
```

---

## 개발

```bash
cargo test --workspace                    # 379 tests
cargo clippy --workspace --all-targets    # 경고 0
cargo fmt --all -- --check
cargo doc --workspace --no-deps
cargo run -p minimal-agent                # 계약 왕복 확인
```

네트워크가 필요한 테스트는 기본 실행에서 빠져 있다. `#[ignore]`와 `RIVET_LIVE` 가드가
이중으로 걸려 있으므로(CI가 `--all-features`라 feature flag만으로는 부족하다) 오프라인
CI가 그대로 통과한다.

```bash
RIVET_LIVE=1 DEEPSEEK_API_KEY=... \
  cargo test -p rivet-model-openai --test live -- --ignored
```

---

## 문서

| 문서 | 내용 |
|---|---|
| [architecture.md](./docs/architecture.md) | 구조적 결정과 **그 이유** |
| [plan.md](./docs/plan.md) | Phase별 작업 계획과 완료 조건 |
| [plugin.md](./docs/plugin.md) | plugin 작성 가이드 |
| [events.md](./docs/events.md) | Session event vs Bus event |
| [task.md](./docs/task.md) | Task 상태 기계와 리뷰 게이트 |
| [security.md](./docs/security.md) | 위협 모델과 방어 계층 |

---

## 설계의 핵심 네 가지

**1. Session event ≠ Bus event.**
전자는 durable fact로 절대 유실되지 않는다. 후자는 알림이며 느린 구독자에게서 유실될 수
있고, 그 유실은 `SubscriberLagged`로 보고된다. 섞으면 결정론적으로 재생할 수 없는 로그가
된다.

**2. 가장 제한적인 결정이 이긴다 — 타입으로.**
합성 규칙을 Core에 박았고, `Interceptor`는 `Allow`를 **표현할 수 없는** 타입을 반환한다.
등록 순서에 따라 달라지는 보안 결정은 보안 결정이 아니다.

**3. Task는 Run보다 오래 산다.**
Run이 죽어도, 리뷰에서 반려돼도, 모델을 바꿔도 Task는 이어진다. 그리고 `Task::state`는
private이다 — 한 줄의 대입으로 무력화되는 게이트는 게이트가 아니다.

**4. 크래시는 예외가 아니라 전제다.**
`append`는 fsync 후에 반환하고, 재개는 답 없이 남은 tool call을 합성 결과로 **로그에**
닫아 세션이 항상 재개 가능하도록 만든다. 취소도 같은 규칙을 따른다 — 한 turn이 세 개의
도구를 요청했고 Ctrl-C가 첫 번째에서 끊었다면, 나머지 둘도 종료 이벤트를 받는다. 그러지
않으면 **취소를 제대로 수행한 대가로 세션이 재개 불가**가 된다.

## 설계 리뷰

Phase 0 산출물은 독립 에이전트의 적대적 설계 리뷰를 **두 번** 거쳤다. 1회전에서 결함
15건, 2회전(수정본 재검증)에서 그 수정이 만든 회귀 5건과 불완전했던 수정 7건이 나왔다.
전부 고쳤고 각각 회귀 테스트가 있다. 목록은
[`docs/plan.md`](./docs/plan.md#설계-리뷰에서-고친-것)에 있다.

2회전이 없었다면 남았을 것들: `.ssh`는 막지만 `.ssh/id_rsa`는 통과하는 deny list,
좁혀 놓은 `--dry-run`이 승인 요구에 삼켜지는 정책 합성, 워크스페이스 밖을 가리키는
`Subtree` 권한, 그리고 영원히 매칭되지 않는 "세션 동안 기억".

---

## 참고

- [Pi Coding Agent](https://github.com/badlogic/pi-mono) — 작은 harness + 확장점
- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) — "Everything is a Plugin"

## 라이선스

Apache-2.0 OR MIT
