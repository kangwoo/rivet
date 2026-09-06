# Task Runtime

> Phase 5 · Rivet의 핵심 차별화 요소

Agent Run은 **한 번의 실행 시도**다. Task는 **목표와 상태를 소유하는 지속 단위**다.
이 둘을 분리했기 때문에 Run이 죽어도, 리뷰에서 반려돼도, 모델을 바꿔도 Task는 이어진다.

```text
Task ──┬── Agent Run #1 (구현)   → Session A → Tool Calls
       ├── Review Run  #1 (검토) → Session B
       └── Agent Run #2 (수정)   → Session C
```

---

## 1. 상태 기계

```text
                    ┌───────────┐
                    │  PENDING  │  생성됨, 의존성 미충족
                    └─────┬─────┘
                          │ 모든 의존성 COMPLETED
                    ┌─────▼─────┐
              ┌────►│   READY   │  스케줄러 슬롯 대기
              │     └─────┬─────┘
              │           │ claim
              │     ┌─────▼─────┐
              │     │  RUNNING  │  Agent Run 진행 중
              │     └──┬──┬──┬──┘
              │        │  │  │
              │  ┌─────┘  │  └──────┐
              │  │        │         │
              │  ▼        ▼         ▼
              │ ┌──────┐ ┌──────┐ ┌─────────┐
              │ │REVIEW│ │FAILED│ │ WAITING │  외부 대기
              │ └──┬───┘ └──────┘ └────┬────┘
              │    │                   │
              │    │ REQUEST_CHANGES   │
              └────┤                   │
                   │ APPROVE           │
              ┌────▼──────┐            │
              │ COMPLETED │◄───────────┘ (의존성 해소 → READY)
              └───────────┘

  CANCELLED ◄── 모든 비종료 상태에서 도달 가능
```

### 전이 규칙

| From | To |
|---|---|
| `PENDING` | `READY` `FAILED` `CANCELLED` |
| `READY` | `RUNNING` `PENDING` `FAILED` `CANCELLED` |
| `RUNNING` | `REVIEW` `WAITING` `READY` `FAILED` `CANCELLED` |
| `WAITING` | `READY` `FAILED` `CANCELLED` |
| `REVIEW` | `COMPLETED` `READY` `FAILED` `CANCELLED` |
| `COMPLETED` `FAILED` `CANCELLED` | — (흡수) |

이 표는 `TaskState::can_transition_to()` **한 곳**에만 존재한다. 스케줄러도 workflow
plugin도 새 전이를 발명할 수 없다.

### 강제되는가

`Task::state`와 `depends_on`은 **private**이다. 공개 필드였다면 이렇게 무력화된다.

```rust
task.state = TaskState::Completed;   // 컴파일되지 않는다
```

유일한 진입점은 검증하는 두 함수다.

```rust
graph.apply(id, TaskState::Completed)?;   // RUNNING 이면 Err
task.transition_to(TaskState::Running)?;  // 예산 소진이면 Err
```

`depends_on`도 같은 이유로 private이다 — 직접 수정하면 `TaskGraph::insert()`의 순환
검사를 우회할 수 있다.

**역직렬화도 같은 문을 통과한다.** 필드를 private으로 만들어도 상태 파일이
`state: "COMPLETED"`나 순환 의존을 주입할 수 있으면 아무것도 얻지 못한다. `TaskGraph`의
`Deserialize`는 `from_tasks()`를 거치며 디스크에서 온 그래프도 메모리에서 만든 것과 똑같이
검증한다.

### 세 가지 결정

**종료 상태는 흡수 상태다.**
`COMPLETED`에서 나가는 경로는 없다. 재개는 새 Task를 만든다는 뜻이며, 그래야 이력이
정직하다. "완료됐다가 다시 실행 중"인 Task는 감사할 수 없다.

**`RUNNING → COMPLETED` 직행은 없다.**
반드시 `REVIEW`를 거친다. 건너뛸 수 있는 게이트는 게이트가 아니다.
리뷰가 필요 없는 워크플로는 `Task::complete_without_review()`를 쓰는데, 이 함수도
`REVIEW`를 **통과**한다. 로그에 "열린 게이트"가 남지 "없는 게이트"가 남지 않는다.

**`WAITING`은 종료가 아니다.**
`is_settled()`는 `WAITING`을 살아있는 상태로 센다. approval-gate 워크플로가 사람 승인을
기다리며 파킹한 순간을 "끝남"으로 보면, Task Runtime이 매번 사람보다 먼저 종료된다.

**`RUNNING → READY`가 있다.**
재시도 경로다. `max_attempts`가 이 순환을 유한하게 만든다.

---

## 2. 재시도 예산

```rust
pub struct Task {
    attempts: u32,
    max_attempts: u32,   // 기본 3
}
```

`RUNNING`에 진입할 때마다 `attempts`가 증가한다. 소진되면 `FAILED`.
이것이 없으면 리뷰 반려 → 수정 → 반려 순환이 토큰을 무한히 태운다.

**예산은 전이 함수에서 강제된다.** `transition_to(Running)`이 소진 상태에서 `Err`를
반환하므로, 어떤 workflow plugin도 예산을 두 번 쓸 수 없다. 스케줄러에만 검사를 두면
스케줄러를 교체하는 순간 사라지는 보호다.

그래서 **`READY → FAILED` 전이가 필요하다.** 이것이 없으면 예산이 소진된 Task는 `READY`에
갇힌다 — `RUNNING`으로는 예산 때문에 못 가고 다른 출구는 없어서, 그래프가 영원히 끝나지
않는다. 예산 강제를 추가하면서 함께 만들어진 교착이었고, 두 번째 리뷰가 잡았다.

---

## 3. Task Graph

DAG를 기본 모델로 한다.

```text
순차                          병렬
Implement API              Implement
     │                      /     \
Run Test                Unit Test  Review
     │                      \     /
Code Review                 Deploy
     │
Deploy
```

### 삽입 시 검증

```rust
graph.insert(task)?;   // 다음을 모두 검사
```

- 알 수 없는 의존성 → `Err`
- 자기 자신 의존 → `Err`
- 순환 (Kahn) → `Err`

**스케줄 시점이 아니라 삽입 시점에 검증한다.** 잘못된 그래프는 새벽 3시의 교착이 아니라
시작 시점의 설정 에러여야 한다.

### 준비와 차단

```rust
graph.newly_ready()   // 의존성이 모두 COMPLETED 인 PENDING task
graph.blocked()       // 의존성 중 FAILED/CANCELLED 가 있는 비종료 task
graph.is_settled()    // 더 이상 진행 불가
```

**실패한 의존성은 후속 Task를 만족시키지 않는다.** 후속은 영원히 `PENDING`으로 남는다 —
그래서 `blocked()`가 필요하다. 이것이 없으면 실패한 leaf 하나 때문에 그래프 전체가
영원히 "작업 중"으로 보인다.

---

## 4. Workflow

```rust
#[async_trait]
pub trait Workflow: Send + Sync + Debug {
    fn name(&self) -> &str;
    async fn next(&self, graph: &TaskGraph) -> Result<Vec<TaskId>>;
    async fn requires_review(&self, graph: &TaskGraph, task: &Task) -> bool;
}
```

Workflow는 그래프 모양에 대한 **순수 정책**이며 그래프를 **변경하지 않는다.**
상태 전이는 Task Runtime이 적용한다. 버그 있는 workflow plugin은 진행을 멈출 수는 있어도
상태를 오염시킬 수는 없다.

`next()`가 빈 벡터를 반환하는 것은 "지금은 없음"이지 "끝"이 아니다.
완료 판정은 `TaskGraph::is_settled()`가 한다.

기본 workflow:

| 이름 | 동작 |
|---|---|
| `sequential` | 한 번에 하나 |
| `parallel` | 준비된 것을 동시성 한도까지 |
| `review-gate` | 모든 Task가 `REVIEW`를 거침 |
| `approval-gate` | 지정된 Task에 사람 승인 요구 |
| `retry` | 실패를 예산 안에서 재시도 |
| `human-in-the-loop` | 매 전이에 확인 |

---

## 5. Scheduler

```rust
#[async_trait]
pub trait Scheduler: Send + Sync + Debug {
    fn name(&self) -> &str;
    fn max_concurrency(&self) -> usize;
    async fn claim(&self, task_id: TaskId) -> Result<bool>;
    async fn release(&self, task_id: TaskId) -> Result<()>;
}
```

`claim()`이 `false`를 반환하면 다른 워커가 이미 가져간 것이다.
**이 원시 연산 하나가 분산 스케줄러의 전제다.** Phase 7에서 계약을 바꾸지 않고 분산
구현을 끼울 수 있어야 하며, 그렇지 않다면 이 계약이 틀린 것이다.

가능한 구현: Local · Cron · Queue · Distributed · Kubernetes.

---

## 6. Review

```text
RUNNING → REVIEW → Reviewer Agent
                      ├── APPROVE          → COMPLETED
                      ├── REQUEST_CHANGES  → READY (재시도)
                      └── REJECT           → FAILED
```

Reviewer도 **일반 Agent다.** 따라서 다른 모델, 다른 도구 집합, 다른 프로파일을 쓸 수
있다.

```toml
[agents.reviewer]
model            = "anthropic/claude-sonnet-5"
tools            = ["read_file", "git_diff", "shell"]   # write_file 없음
profile          = "reviewer"
context_providers = ["system", "task", "git"]
```

리뷰어에게 쓰기 도구를 주지 않는 것이 요점이다. 리뷰어가 코드를 고칠 수 있으면 리뷰가
아니다.

리뷰는 `Task.acceptance`의 체크 가능한 조건에 대해 판정한다.

```rust
Task {
    goal: "로그인 API를 구현한다",
    acceptance: vec![
        "POST /auth/login 이 존재한다".into(),
        "cargo test 가 통과한다".into(),
        "비밀번호가 평문으로 저장되지 않는다".into(),
    ],
    ..
}
```

---

## 7. Demo 시나리오

```text
사용자: "로그인 API를 구현해줘"

Task 그래프 생성
   inspect ──► implement ──► test ──► review ──► deploy

inspect     RUNNING → REVIEW → COMPLETED
implement   RUNNING → REVIEW → COMPLETED
test        RUNNING → REVIEW → FAILED  (테스트 실패)
            └─ workflow: retry 예산 내 → implement 를 READY 로
implement   RUNNING → REVIEW → COMPLETED  (수정)
test        RUNNING → REVIEW → COMPLETED
review      RUNNING → REVIEW → COMPLETED  (Reviewer Agent)
deploy      PENDING (승인 게이트 대기)
```

TUI:

```text
✓ inspect repository
✓ implement login API
✓ run tests
✓ fix failing test
● code review
○ deploy          (approval required)
```

---

## 8. 미해결

- **워크스페이스 동시성** — 한 워크스페이스에서 두 Task가 동시에 실행되면 파일이
  충돌한다. git worktree 분리? 순차 강제? Phase 5 착수 전 결정 필요.
  (`architecture.md` 열린 질문 §11-5)
- **부분 실패 그래프의 재개** — `blocked()` Task들을 어떻게 되살릴 것인가.
  의존성을 고친 뒤 수동 `retry` 명령? 자동?
- **Task 영속화** — 세션과 같은 event-sourced 방식? 별도 상태 저장?
  현재 계약은 둘 다 허용한다. 어느 쪽이든 `ReviewVerdict`는 durable해야 한다 — 세션 승인과
  같은 이유로, "누가 이 Task를 승인했는가"는 감사 질문이다 (Phase 5.9).
