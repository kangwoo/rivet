<!-- Phase 4를 위해 승인받으려는 설계. 구현 전에 쓴다.
     §8은 구현이 이 문서에서 갈라진 지점을 나중에 기록하는 자리이고,
     §8 위의 본문은 나중에 선견지명처럼 보이도록 고치지 않는다. -->

# Phase 4 — Policy / Sandbox: 설계

> 상태: 구현 완료 (설계 리뷰 4라운드 통과) · 구현 대상:
> [`docs/plan.md`](../plan.md) Phase 4 (4.1–4.9, 4.8 제외)
> 이 문서는 **결정과 그 근거**를 남긴다. 무엇을 만들었는지는 커밋이, 계약의 현재 모습은
> [`architecture.md`](../architecture.md) · [`security.md`](../security.md) ·
> [`plugin.md`](../plugin.md) · [`events.md`](../events.md) · [`config.md`](../config.md)가
> 말한다.
>
> 교차 Phase 파급이 있는 열린 질문은 [`architecture.md` §11](../architecture.md#11-열린-질문)로
> 승격한다. §7에 남은 것은 이 Phase 안에서 닫히는 것들이다.

> 범위: `docs/plan.md` §"Phase 4 — Policy / Sandbox"의 작업 항목 4.1–4.7, 4.9와 DoD 6줄,
> 그리고 진행 추적 표가 지목한 게이트(심볼릭 링크 탈출 차단). 4.8은 Phase 1.6b로 옮겨져
> 이미 완료됐고 다시 하지 않는다.
> 기준 트리: `herdr/phase-4-policy-sandbox` (base `4c7bfae`).
> Phase 0에서 확정된 것(Policy 합성은 most-restrictive-wins · `Interceptor`는 허용을 넓힐 수
> 없고 결과는 fold에 합류한다 · 승인은 durable session event다 · 권한 교집합은 부분순서 위의
> `meet`이다 · Session event ≠ Bus event · `ToolContext`는 data와 host로 나뉜다)는 전제이지
> 논의 대상이 아니다.

---

## 1. 문제

Phase 4가 만들 것은 대부분 **이미 있다.** 없는 것은 그것들을 잇는 선이다. 아홉 군데가
비어 있고, 아홉 개가 전부 같은 종류다 — 계약은 완성되어 있고 호출자가 0이다.

첫째, **`dispatch.rs`의 4·5·6·7단계가 주석이다.** 파이프라인 순서는 Phase 1이 고정했고
그 자리에는 `// 4 Intercept, 5 Policy, 6 Approval, 7 Sandbox: Phase 4.` 한 줄이 있다.
둘째, **세 토픽에 발행 지점이 없다.** `tool.policy.evaluated` · `tool.approval.requested` ·
`tool.approval.resolved`는 `every_bus_topic_is_claimed`에서 `Owner::Deferred(4)`로 분류되어
있고, 그 와일드카드 없는 두 층 `match`가 Phase 3이 남긴 컴파일 타임 tripwire다 — Phase 4가
그것을 `Published`로 옮기지 않으면 tripwire가 실패한다. 셋째, **`RuntimeToolHost::exec`가
거절한다.** 몸통이 `Err(PolicyDenied, "sandboxing lands in Phase 4")` 한 줄이고, 그래서
[`plugin.md` §4.5b](../plugin.md)가 "프로세스는 host를 통해 띄운다"고 광고하는 API는 오늘
어떤 인자로도 성공하지 못한다. 넷째, **네 crate가 모듈 doc 세 줄이다.**
`plugins/policy-default` · `plugins/sandbox-local` · `plugins/tool-shell` ·
`plugins/tool-git`은 워크스페이스 멤버이고 `[workspace.dependencies]`에 이름이 있으며
`rivet-cli`의 카탈로그에는 없다 — `rivet.example.toml`이 그 이유를 스스로 적어 뒀다
("아무것도 등록하지 않는 plugin은 `rivet plugin list`에 하는 거짓말이다").

다섯째, **`Profile::permissions()`는 `ProcessSpawn`을 아무에게도 주지 않는다.**
[`security.md` §8](../security.md) 표는 `developer`와 `ci`에 `process ✓`를 약속하고
`reviewer`에 "읽기 명령만"을 약속하는데, 주는 쪽이 없으므로 `process_spawn`을 선언한 plugin은
**모든 프로파일에서** 그 권한이 빈 채로 로드된다. 같은 문서가 그 칸들을 "그렇게 되어야 한다는
진술이지 지금의 상태가 아니다"라고 적고, [`architecture.md` §11-10](../architecture.md)이
"요청자가 생기는 Phase(프로세스는 4) 착수 시점에 명시적으로 연다"고 적어 뒀다. 요청자는
4.5·4.6·4.7이다. 여섯째, **`[sandbox]` 섹션은 inert다.** `Config::load`가 파싱해서
`Inert { sandbox: true }`로 표시하고 `rivet doctor`가 "read, but no confinement is applied
until Phase 4"라고 출력한다. 일곱째, **승인 계약 전체가 `rivet-core` 밖에서 참조 0회다** —
`ApprovalSink` · `ApprovalRequest` · `ApprovalOutcome` · `SessionState::is_pre_approved` ·
`SessionState::remembered_approvals` · `SessionEvent::{ApprovalRequested, ApprovalResolved}`
여섯이 정의되어 있고 아무도 부르지 않는다. 여덟째, **`unattended`가 `DispatchCtx`까지 흘러와서
아무도 읽지 않는다.** `--headless`는 `Config` → `RunConfig` → `DispatchCtx`를 지나
`PolicyRequest.unattended`가 읽을 자리에 도착해 있는데 그 자리를 만드는 코드가 없다.
아홉째, **`Registry::policies()` · `interceptors()` · `sandbox()`의 프로덕션 호출자가 0이다.**
셋 다 테스트에서만 불린다 — `interceptors()`는 `priority` 정렬까지 구현해 두고 한 번도
실행된 적이 없다.

Phase 4는 이 아홉을 닫는다. 그리고 닫으면서 **판정이 두 군데로 갈라지지 않게** 한다:
합성 규칙은 `rivet-core`의 `PolicyDecision::combine`에 이미 있으므로 런타임은 그것을 fold할 뿐
자기 규칙을 만들지 않고, 경로 봉쇄는 `Workspace::resolve`와 `fsguard`에 이미 있으므로 새
샌드박스는 그 둘을 다시 쓴다.

---

## 2. 접근

### 2.1 모양

```text
  ToolCall
     │
  1 Resolve ─ 2 Scope ─ 3 Validate            (Phase 1, 그대로)
     │
     ▼
  ┌──────────────────────── policy_chain::evaluate ────────────────────────┐
  │  seed = 호스트 baseline                                                 │
  │         { timeout · max_output_bytes · grant }   ← sandbox 는 없다      │
  │            │                                                           │
  │  4 Intercept  interceptors()  ──▶ join_all, 각자 2 s 타임아웃           │
  │            │                      멈추면 None + runtime 보고            │
  │  5 Policy     policies()      ──▶ 이름순, 전부 평가                     │
  │            │                                                           │
  │            └──▶ fold: PolicyDecision::combine  (most-restrictive-wins) │
  │                        │                                               │
  │            rewrite 있으면 ─┴─▶ 3·4·5를 재작성된 호출로 다시 (깊이 ≤ 3)  │
  └────────────────────────────────┬──────────────────────────────────────┘
                                   │  publish tool.policy.evaluated
                                   ▼
  6 Approval   RequireApproval 일 때만, 이 순서로:
                 ① 기억된 scope_key ? ─ 예 ─▶ resolved(Approved,  actor=None)
                 ② unattended ?       ─ 예 ─▶ resolved(Denied,    actor=None)
                 ③ 그 외              ─────▶ requested → sink → resolved(actor=Some)
                                   │              (취소 토큰과 select)
                    Deny ──▶ tool.blocked (종료)
                                   │ Allow
                                   ▼
  7 Sandbox    provider 이름 = constraints.sandbox ∨ [sandbox] provider
               레지스트리 조회(싸다) — 없으면 **막지 않고** "샌드박스 없음"으로 기록
                                   │
  8 Execute    ToolHost::exec ──▶ 샌드박스 없으면 여기서 Err ──▶ lazy prepare
                                   │                            ──▶ 프로세스 그룹
  9 Truncate  10 Persist
                                   │
                     ─────── teardown() (Ok · Err · panic · cancel 전부) ───────
```

설계를 지탱하는 결정 여덟.

**(a) 체인은 fold이고, fold의 항등원은 `allow()`가 아니라 호스트의 baseline이다 — 단,
`sandbox` 축은 씨앗에 넣지 않는다.** `tool_timeout_ms`·`max_output_bytes`·프로파일 grant는
전부 `ExecutionConstraints`의 축이고, 그것을 fold의 **씨앗**으로 넣으면 운영자가 설정한 값이
정책과 **같은 규칙**(축별로 더 엄격한 값)으로 합성되며 정책은 그것을 조일 수만 있다.
호스트가 나중에 덮어쓰는 형태였다면 "정책이 30초를 요구했는데 설정이 120초로 되돌렸다"가
가능해진다.

**`sandbox`만 예외인 이유는 `merge`에 있다.** `ExecutionConstraints::merge`의 sandbox 축은
`self.sandbox.or(other.sandbox)`이고 provider 이름 사이에는 **순서가 없다.** fold가
`accumulated.combine(round)` 이므로 씨앗이 항상 `self`이고, 씨앗에 `local`을 넣으면
`docker`를 요구하는 정책이 **진다** — 축별로 더 엄격한 값이 이긴다는 규칙이 이 축에서만
거꾸로 선다. 그래서 설정의 provider는 constraint가 아니라 `DispatchCtx.sandbox_provider`라는
**기본값**으로 따로 나르고, 이 호출의 provider는 `constraints.sandbox ∨ 기본값`이다.
정책이 이름을 대면 그것이 이기고, 아무도 안 대면 설정이 이긴다. 서로 다른 이름 둘이
겹치는 경우는 이 빌드에 provider가 하나뿐이라 관측될 수 없고, §7-2에 열어 둔다.

**(a′) 샌드박스가 없다는 사실은 7단계가 아니라 `exec`에서 실패한다.** 7단계는 **모든** 도구
호출이 지나는 자리이므로, 거기서 "provider가 등록되지 않았다"를 거부로 만들면
`production`(샌드박스 plugin이 0개를 등록한다)에서 `read_file`까지 막히고, `enabled` 목록을
직접 적어 둔 기존 `rivet.toml`은 업그레이드 후 모든 도구 호출이 막힌다. 실패를 거는 자리는
**프로세스를 실제로 띄우려는 순간**이다. 불변식은 이렇게 쓴다:

> **어떤 프로세스도 판정이 요구한 샌드박스 밖에서 뜨지 않는다. 그리고 프로세스를 띄우지
> 않는 호출은 샌드박스가 없다는 이유로 막히지 않는다.**

7단계가 하는 일은 레지스트리 조회 하나(싸다)로 "이 호출에 샌드박스가 실제로 있는가"를
정하고 그것을 `tool.execute.started.sandboxed`에 싣는 것뿐이다. 없으면 기록만 하고 넘어가며,
`ToolHost::exec`가 불리는 순간 provider 이름을 대며 거절한다. 조용하지도 않다 —
`rivet doctor`가 run 전에 같은 조회를 하고, 그 첫 `exec`의 에러가 어느 이름을 못 찾았는지
말한다.

**(b) interceptor는 동시에 돌고 각자 타임아웃을 받는다.** `Registry::interceptors()`가
`priority` → 이름으로 정렬해 주지만, 그 순서가 정하는 것은 [`events.md` §6](../events.md)이
적은 대로 **어느 이유를 먼저 보는가**뿐이다. 결과는 fold이므로 순서와 무관하다. 그러면
직렬로 돌 이유가 없고, 직렬이면 N개의 타임아웃이 더해져 도구 실행 전 대기가 N × 2 s가 된다.
`join_all`로 동시에 돌리면 벽시계 상한이 interceptor 수와 무관하게 2 s이고, fold는 정렬된
순서로 나중에 적용하므로 결정론은 그대로다. **총량 예산을 공유하지 않는 이유**도 같다 —
공유하면 앞선 interceptor가 느릴 때 뒤의 것이 기권하게 되고, 그건 타이밍이 보안 결정을
바꾸는 것이다.

2 s는 **라운드당** 상한이다. 재작성 재평가는 4단계를 다시 돌리므로 호출당 최악의
interceptor 대기는 `REWRITE_DEPTH_LIMIT × INTERCEPTOR_TIMEOUT` = **6 s**다. 그 위에 별도
예산을 두지 않는다 — 그 숫자를 발명하면 §2.2가 `Plugin::load`에 대해 거절한 바로 그 일을
하게 된다. 대신 **체인 전체를 `ctx.cancel`과 `select!`한다**: 취소되거나 run 데드라인이
지나면 체인이 그 자리에서 끝나고 호출은 "시작되지 않았다"로 닫힌다(§5-4와 같은 경로).
6 s는 발명한 예산이 아니라 두 상수에서 **유도된** 상한이고, 그 곱이 커지는 유일한 방법은
두 상수를 키우는 것이다. 이 빌드는 interceptor를 하나도 싣지 않으므로 실측 상한은 0이다.

**(c) 승인 단계의 순서는 ① 기억 ② 무인 ③ 질문이고, `unattended` 변환은 그 ②다.**
`PolicyRequest.unattended`는 정책에게도 보이지만 변환은 **런타임이** 한다
([`architecture.md` §4.3](../architecture.md)). 정책마다 변환하면 어떤 정책은 잊고, 잊은
정책의 `RequireApproval`이 CI에서 매달린다. `Outcome`에 대한 `match`에 와일드카드를 두지
않으므로 변형이 새로 생기면 "무인일 때 이건 뭐가 되는가"를 컴파일러가 묻는다. 변환이
6단계 **안**에 사는 것은 [`architecture.md` §6](../architecture.md)의 파이프라인 표가
6단계를 "`RequireApproval && !unattended` 일 때만"이라고 적은 것과 같은 배치다.

**기억 조회가 무인 변환보다 앞에 오는 것은 의도적이다.** `unattended`는 "지금 아무도 답할
수 없다"이고 기억된 승인은 "이미 누가 답했다"이다. 변환을 먼저 하면 사람이 durable하게
남긴 결정이 현재의 플래그 하나 때문에 버려지고, `--headless`로 resume하는 순간 "세션 동안
기억"이 조용히 무효가 된다. 그 순서는 §7-13에 [가정]으로 남긴다 — 뒤집으면 더 닫히는 쪽이고,
뒤집는 것도 방어 가능하다.

**(d) 승인은 항상 한 쌍으로 남고, `actor: None`이 "사람이 아니라 규칙이 답했다"이다.**
`SessionEvent::ApprovalResolved.actor`의 doc이 이미 그렇게 적어 뒀다 — "`None` for an
automatic resolution (timeout, unattended)". 그 자리는 셋이다: 무인 변환, 기억된 승인,
그리고 대기 중 취소. 셋 다 `approval.requested` → `approval.resolved` 쌍을 남긴다. 쌍을
깨고 "물어본 경우에만 requested를 쓴다"로 하면 로그를 읽는 쪽이 "이 호출은 승인이 필요했나"를
`tool.blocked`의 문구에서 역추적해야 하고, 감사에서 필요한 것은 정확히 그 사실이다.
대가는 `ApprovalRequested`의 doc 한 줄을 "사람에게 물었다"에서 "이 호출은 승인 판정을
거쳤다"로 고치는 것이고, 그렇게 한다.

**(e) 샌드박스 handle은 디스패처가 소유한다. 도구 태스크가 아니다.** 이것이 DoD 4의 전부다.
`dispatch.rs`의 `execute`는 유예 시간이 지나면 도구 태스크를 **abort하지 않고 버린다**
(중간에 abort하면 반쯤 쓰인 파일이 남기 때문이다). 버려진 태스크가 handle을 들고 있으면
`teardown()`을 부를 주체가 사라지고 자식 프로세스는 고아가 된다. handle을 디스패처가 들면
버려진 태스크가 무엇을 하든 **디스패처가 프로세스 그룹을 죽인다** — 즉 "취소 시 좀비 없음"은
샌드박스의 성질이 아니라 소유권의 성질이다. 패닉 경로는 Phase 2의 로더가 쓰는 모양을 그대로
쓴다: 8·9단계를 `AssertUnwindSafe(..).catch_unwind()`로 감싸고 그 뒤에 `teardown().await`을
둔다. `Drop`으로 하지 않는 이유는 [`sandbox.rs`](../../crates/rivet-core/src/sandbox.rs)가
스스로 적어 뒀다 — `Drop`은 await할 수 없다. 대신 `teardown` 없이 drop된 handle은
`tracing::error!`를 남긴다. 그 문장 역시 계약에 이미 있다.

정확히 말하면 **공유 소유**다. `RuntimeToolHost`가 `Arc<SandboxScope>`를 들고 버려진 태스크가
그 `Arc`를 계속 잡고 있으므로 디스패처는 값을 회수할 수 없다 — `teardown`은 `&self`를 받고
멱등이다(`SandboxHandle::teardown`이 이미 `&self`이므로 계약 쪽 변경도 없다). 그리고 handle은
`Mutex` 안에 `Arc`로 산다: `exec`가 락 안에서 clone해 꺼내고 **락을 놓은 뒤** 자식을 기다리므로,
`teardown`이 같은 뮤텍스에서 자식의 수명만큼 막히는 일이 없다 (§4.2). 소유권으로 얻은
불변식을 락으로 되돌리지 않는다. 그리고 teardown 뒤의 scope는 **봉인된다**: 버려진 태스크가 `exec`를 다시 부르면
새 handle을 만드는 대신 `Err`가 나간다. 이 빌드에서는 자식 토큰이 이미 취소돼 있어 새로 뜬
자식이 곧 죽지만, 같은 코드가 겨냥하는 `docker`에서는 teardown 뒤에 컨테이너가 하나 더 뜨고
그것을 내릴 주체가 없다.

**(f) 봉쇄는 셸에 대해 강제되지 않는다. 강제되는 것은 경로 인자와 cwd다.**
`sandbox-local`은 `filesystem_isolation: false`를 신고하고, 그건 `sh -c "cat ../../.env"`를
막을 방법이 없다는 뜻이다. 이 설계는 그걸 막는 척하지 않는다. 대신 세 가지를 한다.
① 도구 호출의 **경로형 인자**(`path` · `cwd`)는 `default.workspace` 정책이 어휘적으로 거르고,
② `cwd`는 `sandbox-local`이 `fsguard::resolve_dir`로 **열고 나서** 다시 검사한다(심볼릭 링크
게이트, 진행 추적 표의 Phase 4 게이트가 여기 있다), ③ `shell`은 애초에 **쓰기 권한이 있는
프로파일에만 등록된다** — `.git/config` 쓰기 권한이 셸 권한과 같다는
[`security.md` §3](../security.md)의 문장을 거꾸로 읽으면, 셸 권한은 쓰기 권한이다.
파괴적 명령 매처는 그 위에 얹는 **UX 장치**이지 경계가 아니고, 문서에 그렇게 적는다.

**(g) 프로파일 grant는 등록층뿐 아니라 정책층에서도 강제된다.** Phase 2가 만든 것은
"`readonly`에서 `write_file`은 **등록되지 않는다**"이고, 그건 "다른 plugin이 같은 이름으로
등록하면?"에 답하지 못한다. `Tool` 계약에는 "이 도구가 요구하는 permission" 필드가 없고
(`ToolSpec`은 이름·설명·스키마·annotations뿐이다), `tool-filesystem`은 `fs_write`를 **등록
시점에만** 보고 실행 시점에는 `ctx.permissions()`를 아예 읽지 않는다. 그래서 강제로 등록된
`write_file`은 오늘 `readonly`에서 그냥 돈다. Phase 4는 그 자리를 세 번째 기본 정책
`default.grant`로 닫는다 — **변이한다고 스스로 선언한 도구(`annotations.read_only == false`)는
`FsWrite`가 없는 grant 아래에서 돌 수 없다.** annotations는 자기 신고이고 거짓말할 수 있지만,
[`security.md` §5](../security.md)가 in-process plugin을 신뢰된 코드로 못박았으므로 이 층이
막는 것은 *악의*가 아니라 *실수*다 — `ToolAnnotations`의 doc이 스스로 "그래서 기본 정책이
도구 목록을 손으로 관리하지 않고도 엄격할 수 있다"고 적어 둔 용도가 정확히 이것이다.
이것이 DoD 6("`readonly`에서 `write_file`이 **실제로** 거부됨")의 정책층이다.

### 2.2 기각한 대안

| 대안 | 기각 이유 |
|---|---|
| **`[sandbox] provider`를 fold의 씨앗(`constraints.sandbox`)에 넣는다** | `merge`의 sandbox 축은 `self.or(other)`이고 provider 이름에는 순서가 없다. 씨앗은 언제나 `self`이므로 `local`을 씨앗에 넣으면 `docker`를 요구하는 정책이 **진다** — 나머지 축과 정반대다. 기본값은 constraint가 아니라 `DispatchCtx`의 필드로 나른다 (§2.1-a). |
| **호스트가 `[sandbox] provider`를 체인 **뒤에** 덮어쓴다** | 그러면 정책이 `docker`를 요구해도 설정이 `local`로 되돌린다. 정책이 이름을 대면 그것이 이기고, 아무도 안 대면 설정이 이긴다 — 방향이 하나여야 한다. |
| **`policy-default`가 `constraints.sandbox`를 채운다** | 그러면 그 plugin을 끄는 것이 프로세스 실행을 조용히 비격리로 만든다. 격리 여부는 정책 plugin의 존재에 달려서는 안 되고, `[sandbox]`를 읽은 것은 호스트다. |
| **7단계에서 provider 미등록을 거부로 만든다** | 7단계는 **모든** 호출이 지나는 자리다. `production`은 `sandbox-local`을 0개 등록하므로 `read_file`까지 막히고, `enabled`를 직접 적어 둔 기존 `rivet.toml`은 업그레이드만으로 모든 도구 호출이 막힌다. 실패는 프로세스를 띄우려는 순간(`ToolHost::exec`)에 건다 (§2.1-a′). |
| **샌드박스가 없으면 프로세스를 비격리로 띄운다** | 반대 방향의 실패이고 더 나쁘다. 판정이 요구한 격리 밖에서 프로세스가 뜨는 경우는 없어야 한다 (§2.1-a′의 불변식). |
| **interceptor를 직렬로, 총량 예산 하나로 돌린다** | 앞선 interceptor의 지연이 뒤의 것을 기권시킨다 — 타이밍이 보안 결정을 바꾼다. 개별 타임아웃 + 동시 실행이면 각자 답하거나 각자 기권한다 (§2.1-b). |
| **interceptor가 `Some`이면 단축한다** | Phase 0이 이미 기각했고 `RestrictiveDecision`에 `Allow`가 없는 이유다. 단축은 등록 순서를 보안 결정으로 만든다. |
| **`Plugin::load`/`unload` 타임아웃도 같은 모양으로 함께 넣는다** | 넘긴다. [`§11-15`](../architecture.md)가 열어 둔 것은 **숫자가 아니라 초과했을 때 남길 상태**(`FAILED` 레코드인가 호스트 중단인가)이고, interceptor는 그 답이 계약에 이미 쓰여 있다 — "멈춘 interceptor는 `None`으로 처리되고 보고된다". 답이 있는 쪽의 숫자는 발명해도 대가가 기권 하나이고, 답이 없는 쪽의 숫자는 부하 걸린 CI에서 느린 `load`를 `FAILED`로 만든다. 같은 모양이 아니다 (§5의 "알려진 미해결"). |
| **무인일 때 `approval.*`를 아예 남기지 않는다** | "모델이 승인 대상을 요청했고 CI가 거부했다"가 로그에서 사라진다. 남는 것은 `tool.blocked` 하나이고, 그 사유 문자열을 파싱해야 승인 때문임을 안다 (§2.1-d). |
| **기억된 승인을 메모리에만 둔다** | resume 후 동작이 달라지고 감사에 안 나타난다. [`security.md` §7](../security.md)이 명시적으로 금지한다. `SessionState::remembered_approvals`가 로그의 projection인 이유가 그것이고, Phase 4는 그 projection을 **읽기만** 한다. |
| **`scope_key`를 `ToolCallId`로 만든다** | Phase 0 리뷰 2라운드가 이미 잡았다. 다음 호출마다 새 id이므로 영원히 매칭되지 않는다. |
| **`scope_key`를 명령 전문(`shell:rm -rf build`)으로 만든다** | 인자 하나만 달라도 다른 키다. 그러면 기억이 동작하지 않는 것은 물론이고 **로그에서 묶이지도 않는다** — "이 세션에서 `rm` 게이트가 몇 번 걸렸나"에 답할 수 없다. 프로그램 이름(`shell:rm`)이 사람이 승인하거나 세는 단위다. 반대 극단인 `shell:*`은 세션 동안 셸 전체를 내주는 것이라 역시 아니다. 이 빌드에서 셸 키는 **언제나 `allow_remember: false`와 함께** 나오므로(§3.3) 기억되는 일이 없고, 그때까지 이 키가 하는 일은 로그의 묶음이다. |
| **DoD 6을 등록층만으로 증명한다** | 그 증명은 "다른 plugin이 같은 이름의 도구를 등록하면?"에 답하지 못하고, 오늘 그 답은 "그냥 돈다"이다 — `tool-filesystem`은 실행 시점에 `ctx.permissions()`를 읽지 않는다. `default.grant`가 그 자리를 닫는다 (§2.1-g). |
| **대신 `tool-filesystem`이 실행 시점에 `ctx.permissions()`를 검사하게 한다** | 도구마다 같은 검사를 다시 짜게 되고, 그중 하나는 반드시 틀린다 — 스키마 검증을 정책 앞에 둔 이유와 같다([`architecture.md` §6](../architecture.md)). 게다가 그 검사를 안 하는 **다른** plugin의 도구는 여전히 통과한다. 판정은 파이프라인의 한 자리에 있어야 한다. |
| **샌드박스 handle을 도구 태스크가 소유한다** | 유예 시간 뒤 버려진 태스크와 함께 handle이 사라지고 자식이 고아가 된다 (§2.1-e). DoD 4가 정확히 이걸 금지한다. |
| **`SandboxHandle`을 `Drop`으로 해제한다** | `Drop`은 await할 수 없다. 계약이 이미 "런타임의 의무"라고 적어 뒀고, 그 문장은 Phase 0 리뷰가 고친 결과다. |
| **취소 시 자식을 `Child::kill()`로 죽인다** | 손자가 남는다. `sh -c "cargo test"`의 `cargo`는 자식이고 `rustc`는 손자다. 프로세스 **그룹**을 죽여야 하고, 그래서 spawn 시점에 `process_group(0)`이 필요하다. |
| **`libc::killpg`를 `unsafe` 블록으로 부른다** | 워크스페이스가 `unsafe_code = "forbid"`이고, `rivet-runtime`이 `libc`를 **상수만** 쓰는 것도 같은 이유다(`fsguard`의 주석이 그렇게 적는다). 안전 래퍼를 쓴다 (§3.4). |
| **셸 명령 문자열에서 `.env`·`.ssh` 같은 deny 패턴을 찾아 거부한다** | 문자열 매칭은 경계가 아니다. `cat .en''v`가 통과하고, 통과했다는 사실이 "봉쇄된다"는 잘못된 인상을 남긴다. 셸에 대한 답은 **누가 셸을 받는가**이지 무엇을 타이핑했는가가 아니다 (§2.1-f). |
| **모든 `shell` 호출에 승인을 요구한다 (fail closed)** | `ci` 프로파일이 쓸모없어진다 — 무인 변환이 모든 셸 호출을 거부하므로 `cargo test`도 못 돈다. `ci`에 `process ✓`를 약속한 표와 정면으로 충돌한다. 파괴적 형태만 요구하고, 매처가 경계가 아님을 문서에 적는 쪽을 택한다. |
| **`annotations.destructive`만으로 승인을 건다** | `write_file`이 `destructive: true`이므로 `developer`에서 파일 쓰기마다 묻게 된다. [`security.md` §8](../security.md)은 `developer`를 "파괴적 작업만"으로 적었고, 워크스페이스 안의 파일 쓰기는 봉쇄가 이미 경계를 그은 작업이다. 그래서 조건에 "워크스페이스로 한정되지 않는"을 붙인다 (§3.3). |
| **`ExecOutput.stdout`을 `Vec<u8>`로 바꾼다** ([`§11-7`](../architecture.md)) | 계약의 wire 표현이 바뀌고 모든 소비자가 영향을 받는데, 얻는 것은 모델이 어차피 쓸 수 없는 바이트다. lossy 변환을 유지하되 **조용히 하지 않는다** — 치환이 일어나면 `ToolResult::structured`에 `lossy: true`가 실린다 (§3.5). |
| **`ProcessSpawn`을 `developer`·`ci`에만 준다** | `reviewer`가 `git_diff`를 못 쓴다. `rivet.example.toml`의 `[agents.reviewer]`가 `git_diff`·`git_log`를 자기 도구 목록에 적고 있고, [`security.md` §8](../security.md)은 `reviewer`에 "읽기 명령만"을 약속한다. 권한 어휘가 "읽기 명령만"을 표현하지 못하므로 **도구가** 그 구분을 진다 (§3.6). |
| **`CapabilityKind::Interceptor` 변형을 추가한다** ([`§11-11`](../architecture.md)) | 닫힌 어휘를 넓히는 `rivet-core` 변경이고 모든 매니페스트의 wire 표현에 영향을 준다. 그런데 이 빌드가 싣는 plugin 중 interceptor를 등록하는 것은 **없다** — Phase 4가 만드는 것은 실행 경로이지 요청자가 아니다. 요청자가 없는 어휘 확장은 하지 않는다. 기각 이유가 Phase 2의 "범위 밖"에서 "요청자 없음"으로 **바뀐 것**을 `plugin.md`에 적는다. |
| **`tool.policy.evaluated`를 `Allow`가 아닐 때만 발행한다** | 관측자가 "왜 이게 그냥 실행됐는가"에 답할 수 없다. 토픽이 나르는 것은 결정이지 거부가 아니고, 거부 전용 토픽은 `tool.blocked`로 이미 있다. |
| **정책 denial이 run을 `StopReason::PolicyBlocked`로 끝낸다** | DoD 5가 정반대를 요구한다 — 거부된 도구는 세션에 남고 **모델이 사유를 본다**. 모델이 적응할 수 있는 것을 런타임이 끝내면 `rivet --profile readonly "delete all logs"`가 "못 한다"는 답 대신 exit 4가 된다 (§7-5). |
| **`policy-default`가 프로파일 이름을 코드에 박는다** | `production`에서 전부 승인이라는 규칙이 plugin 코드에 박히면 운영자가 바꿀 수 없다. plugin 설정 키(`require_approval_for_all_in`)로 내리고 기본값을 `["production"]`로 둔다. 결합은 남지만 **설정의 결합**이다. |
| **`shell`을 `default_selection`에서 뺀다** | `docs/plan.md`의 MVP 경계 표가 `filesystem · shell · git`을 MVP에 두고, `rivet.example.toml`이 네 plugin을 "Phase 4가 실을 때까지" 빼 뒀다고 스스로 적는다. 관측 사이드카(Phase 3의 `telemetry-log`)와 달리 이건 에이전트를 돌리는 데 쓰이는 도구다. 대신 **프로파일이** 누가 받는지 정한다. |
| **`tool-shell`이 `production`에서 `Err`로 로드를 실패시킨다** | `rivet --profile production`이 아예 시작하지 못한다. `catalog::load`는 어느 plugin의 실패든 전체 실패로 바꾼다. 0개 등록으로 내려앉고 `rivet plugin show`가 어느 권한이 깎였는지 말한다 (§5-14). |
| **`ToolSpec`을 계속 호출 시점에 읽는다** ([`§11-14`](../architecture.md)) | Phase 4에서 **정책이 `annotations`를 읽기 시작한다.** 그러면 `validate_spec`이 검사한 spec과 정책이 판단한 spec이 다를 수 있다 — plugin이 `Arc<dyn Tool>`을 계속 들고 있으면 등록 뒤에 바꿀 수 있기 때문이다. 등록 시점에 **고정**한다 (§3.2). |

---

## 3. 구체적 변경

### 3.1 `crates/rivet-core` — 문서 세 줄과 계약 0줄

타입도, wire 표현도, 트레이트 시그니처도 바뀌지 않는다. 고치는 것은 세 개의 doc이다.

- **`src/session.rs`** — `ApprovalRequested`의 doc "A human was asked to authorize
  something"을 "이 호출은 승인 판정을 거쳤다. 누가 답했는지는 `ApprovalResolved.actor`가
  말하고, `None`이면 사람이 아니라 규칙이 답한 것이다"로 바꾼다 (§2.1-d).
- **`src/tool.rs`** — `ToolContextData.permissions`의 "already narrowed by profile and
  policy"가 Phase 4부터 참이 된다. 어디서 좁혀지는지(`ExecutionConstraints.permissions`)를
  적는다.
- **`src/sandbox.rs`** — `ExecSpec.env`의 "빈 환경에서 시작한다"에 각주를 단다: 운영자가
  `[plugins."rivet.sandbox-local"] env_passthrough`로 **명시한** 이름만 provider가 채운다.
  "누군가 그것을 명시적으로 적어야 한다"는 문장의 그 "적는 자리"가 어디인지 이제 존재한다.

`Outcome`·`RestrictiveDecision`·`ApprovalOutcome`·`ExecutionConstraints`는 그대로 쓴다.
Phase 0이 세 필드로 나눠 둔 `PolicyDecision`과 `combine`이 이 Phase의 fold 그 자체다.

### 3.2 `crates/rivet-runtime` — 체인·승인·샌드박스 경계 (4.1 · 4.2 · 4.4의 런타임 절반)

- **`src/policy_chain.rs` (신규)** — 4.1과 4.2.
  - `Baseline` : 호스트가 넣는 씨앗 constraint. **`sandbox` 축은 비어 있다** (§2.1-a).
  - `evaluate(registry, spec, request, baseline, cancel) -> Evaluated` : interceptor를
    `join_all` + 개별 `tokio::time::timeout(INTERCEPTOR_TIMEOUT)`으로 돌리고, 정책을 이름순으로
    전부 평가하고, 둘을 하나의 fold에 넣는다. interceptor의 `Err`와 타임아웃은 `None`으로
    접고 **보고한다**(`tracing::warn!` + `RuntimeEvent`는 만들지 않는다 — 새 토픽은 어휘
    확장이다, §7-4).
    **`spec: &ToolSpec`이 인자인 이유는 재작성 때문이다.** 이 체인은 재작성된 호출로
    3단계를 다시 돌리겠다고 약속하는데(§2.1의 그림, 그리고 `PolicyDecision::rewrite`의
    계약 doc이 "스키마 검증과 정책 체인 전체를 다시 돌린다"고 적는다), `PolicyRequest`는
    `call`과 `annotations`만 나르고 `input_schema`가 없다
    ([`policy.rs:20-27`](../../crates/rivet-core/src/policy.rs)) — 재료 없이 약속만 하는
    시그니처였다. spec은 §3.2가 등록 시점에 고정하는 바로 그 값이고, `dispatch`가 이미
    손에 들고 있으므로 새로 조회할 것도 없다.
  - `Evaluated { decision, deciding: String, rounds: u8 }` : `deciding`은 최종 outcome을
    만든 정책/interceptor의 이름이고 `tool.policy.evaluated.policy`와 `ToolBlocked.policy`에
    같은 값이 들어간다. fold가 이름을 나르지 않으므로 체인이 `(name, decision)` 쌍을 접는다.
    **아무도 결정하지 않았을 때의 값도 정해 둔다**: 정책이 0개이거나 전부 `Allow`를 주면
    최종 outcome을 만든 것은 씨앗이므로 `deciding = "host.baseline"`이다. 빈 문자열이나
    `Option`이 아닌 이유는 `tool.policy.evaluated.policy`가 `String`이고(계약이 그렇다),
    관측자가 "이 호출은 왜 그냥 실행됐는가"에 답할 수 있어야 하기 때문이다 — §2.2가
    "`Allow`가 아닐 때만 발행한다"를 기각한 것과 같은 이유다. 이 이름은 어느 plugin도
    쓸 수 없다: `Policy::name`이 `default.`/`host.` 네임스페이스를 강제하지는 않지만,
    `host.baseline`이라는 이름을 등록하는 plugin은 §6.3의
    `no_registered_policy_may_claim_the_baseline_name`이 잡는다.
  - 재작성 수렴 루프: `rewrite_is_wellformed` 실패는 즉시 `Deny`, 성공하면 재작성된 호출로
    3·4·5를 다시 돌리고 각 라운드의 결정을 rewrite를 뺀 채 누적한다. `REWRITE_DEPTH_LIMIT = 3`을
    넘기면 `Deny`("정책 체인이 재작성으로 수렴하지 않았다"). 별도 시간 예산은 없다 —
    체인 전체가 `cancel`과 `select!`되고, 상한은 두 상수의 곱에서 유도된다 (§2.1-b, §5-9b).
  - `apply_unattended(decision, unattended) -> PolicyDecision` : 와일드카드 없는 `match`.
    **`Approvals`가 기억을 조회한 다음에** 부른다 (§2.1-c).
- **`src/approval.rs` (신규)** — 4.4의 런타임 절반.
  - `Approvals { sink: Option<Arc<dyn ApprovalSink>>, remembered: Arc<Mutex<BTreeSet<String>>> }`.
    `remembered`는 run 시작 시 `SessionState::remembered_approvals()`로 **씨를 뿌린다** —
    그것이 DoD 3의 전부다. 새 grant는 `ApprovedForSession`을 기록한 뒤 집합에 넣는다.
  - `resolve(&self, ctx, session, call, outcome_fields) -> ApprovalOutcome` : 6단계 전체다.
    ① `remembered`에 `scope_key`가 있으면 sink를 부르지 않고 `Approved`(`actor: None`),
    ② 아니면 `unattended` **이거나 sink가 없으면** `Denied`(`actor: None`), ③ 아니면 sink를
    부른다. ②의 "sink가 없으면"은 임베더를 위한 것이다 — CLI는 §3.7이 `unattended`를
    `config.unattended || sink.is_none()`으로 계산하므로 그 조합을 만들지 않지만,
    `rivet-runtime`을 직접 쓰는 호스트와 테스트 하네스는 만들 수 있고, 그때 물을 곳 없는
    승인이 조용히 통과하면 안 된다. 닫히는 쪽으로 정한다.
    어느 갈래든 durable 쌍을 쓰고 버스에 두 토픽을 발행한다. sink 대기는
    `tokio::select!`로 `ctx.cancel`과 경쟁하고, 취소되면 `TimedOut`으로 해소한다 — 그 변형의
    doc이 "Nobody answered in time"이고 run 데드라인이 지나간 것이 정확히 그 뜻이다.
    **승인 자체에는 별도 타임아웃을 두지 않는다**: run 데드라인(`max_duration_ms`)이 이미
    `ctx.cancel`을 통해 도달하므로 발명할 숫자가 없다.
- **`src/sandbox_scope.rs` (신규)** — 7단계.
  - `SandboxScope { name: Option<String>, provider: Option<Arc<dyn Sandbox>>,
    request: SandboxRequest, state: Mutex<ScopeState> }`,
    `ScopeState = Idle | Prepared(Arc<dyn SandboxHandle>) | TornDown`.
    디스패처가 `Arc<SandboxScope>`로 들고 같은 `Arc`를 `RuntimeToolHost`에 넘긴다 (§2.1-e).
    **handle이 `Box`가 아니라 `Arc`인 것이 이 자료구조의 요점이다**: `exec`는 락 안에서
    `Arc`를 clone해 꺼낸 뒤 **락을 놓고** 자식을 기다린다. `Box`를 뮤텍스에 두면 빌리기 위해
    가드를 자식이 끝날 때까지 들고 있어야 하고, 그 사이 `teardown(&self)`이 같은 뮤텍스에서
    막힌다 — §2.1-e가 세운 불변식의 정반대다. `Sandbox::prepare`는 `Box`를 돌려주지만
    `Arc::from(boxed)`로 바로 받는다.
  - 7단계는 이름(`constraints.sandbox` ∨ `ctx.sandbox_provider`)을 정하고 레지스트리를 **조회만**
    한다. 조회 실패는 **거부가 아니다** — `provider: None`으로 기록되고 `sandboxed: false`가
    발행된다 (§2.1-a′). handle은 첫 `exec`에서 만든다: `docker`의 `prepare`는 싸지 않고,
    대부분의 호출은 프로세스를 띄우지 않는다.
  - `exec(&self, spec, cancel)` : `Idle`이면 `prepare` 후 실행, `Prepared`면 그대로 실행,
    `TornDown`이면 `Err(Cancelled)`. `provider`가 `None`이면 이름을 대며 `Err(NotFound)` —
    **여기가 "샌드박스 없음"이 실패가 되는 유일한 자리다.**
  - `teardown(&self)` : `Prepared`면 `handle.teardown().await` 후 `TornDown`으로 봉인,
    아니면 상태만 `TornDown`. 멱등이고 `&self`다 — 버려진 도구 태스크가 `Arc`를 잡고 있으므로
    값을 회수할 수 없다.
  - `Drop` : 상태가 `Prepared`인 채로 떨어지면 `tracing::error!`. 계약이 "worth logging
    loudly"라고 적은 그 자리다.
- **`src/dispatch.rs`** — 주석 네 줄을 실제 4·5·6·7단계로 바꾼다. `DispatchCtx`에
  `sandbox_provider: Option<String>`, `approvals: Approvals`가 붙는다. `execute`는
  `Arc<SandboxScope>`를 만들어 `RuntimeToolHost`에 넘기고, 8·9단계를
  `AssertUnwindSafe(..).catch_unwind()`로 감싼 뒤 **모든 경로에서** `scope.teardown().await`.
  `RuntimeToolHost::exec`는 거절 대신 `scope.exec(spec, cancel)`로 위임한다 — 거절 문구는
  "샌드박스 provider `<이름>`이 등록되어 있지 않다"로 바뀌고, 그 문구가 나오는 경우는
  §2.1-a′가 정한 하나뿐이다.
  `WORKSPACE_POLICY` 상수는 남는다 — **도구가 직접 올린** `PolicyDenied`(즉 `fsguard`)의
  이름이고, 체인이 만든 거부는 `deciding`을 나른다. 그 구분을 모듈 doc에 적는다.
- **`src/registry.rs`** — 둘이다. 하나: `register_policy`가 `BASELINE_POLICY`
  (`"host.baseline"`)라는 이름을 **거부한다.** 그 이름은 `Evaluated.deciding`이 "아무도
  결정하지 않았고 씨앗이 결정했다"를 말할 때 쓰는 값이므로(위), 그것을 등록할 수 있으면
  `tool.policy.evaluated.policy`와 `ToolBlocked.policy`가 거짓말할 수 있다. 이름 충돌 거부는
  이 테이블이 이미 하는 일이고(Phase 0의 "등록은 소유권 추적, 이름 충돌 거부"), 이건 호스트가
  자기 이름 하나를 미리 차지하는 것이다. 둘:
  `register_tool`이 검증한 `ToolSpec`을 **테이블에 함께 넣는다**.
  `tool(name)`이 `RegisteredTool { tool: Arc<dyn Tool>, spec: Arc<ToolSpec> }`를 돌려주고,
  `tool_specs()`가 모델에게 보낼 목록을 준다. 반환형이 바뀌므로 `.spec()` 호출 지점 넷을
  고친다: `dispatch.rs:204`, `agent_loop.rs:366`, `registry.rs:397`, 그리고
  `examples/minimal-agent/src/main.rs:60-61` — 예제도 워크스페이스 멤버라
  `clippy --all-targets`에 걸린다. 빌드가 즉시 알려주므로 위험은 아니지만 변경 목록에는 있어야
  한다. [`§11-14`](../architecture.md)가 Phase 4로 미뤄 둔 결정이고, 지금 닫는 이유는 이
  Phase에서 **정책이 `annotations`를 읽기 시작하기 때문**이다.
- **`src/agent_loop.rs`** — `RunConfig`에 `sandbox_provider`, `approvals`(또는 그 재료)가
  붙고 `run_tools`가 `DispatchCtx`에 실어 나른다. `run()`이 `state.remembered_approvals()`로
  `Approvals`의 씨를 뿌린다. 그 외 루프 구조는 바뀌지 않는다.
- **`tests/policy.rs` · `tests/approval.rs` · `tests/sandbox.rs` (신규)** — §6.

### 3.3 `plugins/policy-default` (4.3)

매니페스트: `capabilities = ["policy"]`, 권한 없음. **정책은 순수 함수이므로 권한이 필요
없다** — 파일시스템도 네트워크도 보지 않고 `PolicyRequest`만 본다. 세 개의 `Policy`를 등록한다.

**`default.workspace`** — 호출 입력의 **경로형 인자**를 `request.workspace.resolve`에 태운다.
경로형 키는 선언된 집합(`path`, `cwd`)이고, 문자열이면 검사하고 배열이면 원소마다 검사한다.
탈출이나 deny 목록 적중은 `Deny`이고 사유가 어느 키였는지 말한다. 이것은 `fsguard`를
대체하지 않는다 — `fsguard`는 연 **뒤에** 검사하고 이건 열기 **전에** 검사하며, 그래서
파일을 열지 않는 도구(`git`, `shell`의 `cwd`)에도 적용된다. 어떤 인자가 경로인지 스키마가
말해주지 않는다는 점은 §7-1에 열어 둔다.

**`default.grant`** — 프로파일 grant를 **실행 시점에** 강제한다 (§2.1-g). 규칙 하나다:

> `annotations.read_only == false`인 호출은 `request.permissions`에
> `FsWrite(_)`가 없으면 `Deny`한다.

사유는 어느 프로파일이 무엇을 뺐는지 말한다("`readonly`는 `fs_write`를 주지 않는다;
`write_file`은 스스로 변이한다고 선언한다"). 이것이 DoD 6의 정책층이고, 등록층
(`tool-filesystem`이 `fs_write` 없으면 `write_file`을 등록하지 않는 것)과 **독립적으로**
성립한다 — 다른 plugin이 같은 이름으로 등록해도, 아예 다른 이름의 쓰기 도구를 들여와도
같은 규칙에 걸린다. 판정은 `granted().iter().any(|p| matches!(p, Permission::FsWrite(_)))`이지
`allows(&FsWrite(Workspace))`가 **아니다.** `allows`는 요청이 grant에 덮이는지를 보므로
(`allows_respects_scope_ordering`이 같은 성질을 `FsRead`로 단언한다),
`FsWrite(Subtree("src"))`만 가진 grant는 `allows(&FsWrite(Workspace))`에 `false`를 준다 —
좁은 쓰기 권한을 가진 프로파일이 "쓰기 권한이 없다"로 읽힌다. 좁은 것은 좁은 대로 쓰기이고,
어느 경로인지는 `default.workspace`와 `fsguard`의 몫이다. 다섯 프로파일 중 subtree write를
주는 것은 없으므로 오늘의 동작은 어느 쪽이든 같지만, 틀린 쪽을 적어 두면
§6.3의 `a_subtree_write_grant_still_counts_as_write`가 실패한다.

**한계를 적어 둔다. 두 갈래이고, 반대 방향으로 틀린다.**

*지나가는 쪽.* annotations는 자기 신고이므로 `read_only: true`라고 거짓말하는 plugin은
이 층을 지난다. [`security.md` §5](../security.md)가 in-process plugin을 신뢰된 코드로
못박았으므로 이 층이 막는 것은 실수이지 악의가 아니고, `ToolAnnotations`의 doc이 그 용도를
스스로 적어 뒀다. 진짜 답은 `ToolSpec`이 "이 도구가 요구하는 permission"을 선언하는 것인데,
그건 계약 변경이라 §7-14에 열어 둔다.

*막히는 쪽 — 이쪽이 실제로 더 자주 부딪힌다.* `ToolAnnotations::default()`는
`read_only: false`이고 `ToolSpec::new`가 그 기본값을 쓴다
([`tool.rs:63`·`:80-85`](../../crates/rivet-core/src/tool.rs)). 그래서 이 규칙 아래에서
막히는 것은 *거짓말하는 도구*뿐이 아니라 **annotations를 그냥 안 적은 도구 전부**다 —
`readonly`·`reviewer`·`production`에서 그런 도구는 하나도 돌지 않는다. 방향은 맞다(모르는
것을 변이한다고 보는 것이 fail-closed다) 그리고 그것이 이 기본값이 `false`인 이유이기도
하지만, **말없이 그렇게 되면 안 된다**: 거부 사유가 "이 도구는 `read_only`를 선언하지
않았다"를 명시적으로 말하고, `plugin.md`에 "읽기 전용 도구는 `read_only: true`를 적어야
`readonly` 계열 프로파일에서 돈다"를 넣는다 (§3.9). 이 빌드가 싣는 읽기 도구 셋은 전부
적어 뒀다(`read_file.rs:52`·`list_dir.rs:45`·`search.rs:75`). 이 갈래를 붙드는 테스트는
§6.3의 `a_tool_that_declares_no_annotations_is_treated_as_mutating`이다.

**`default.destructive`** — 아래 순서로 판정한다. 각 절은 `RequireApproval`을 만들고,
fold가 나머지와 합성한다.

| 조건 | 결과 | `scope_key` | `allow_remember` |
|---|---|---|---|
| `profile ∈ require_approval_for_all_in` (기본 `["production"]`) | `RequireApproval` | `<tool>` | `true` |
| `shell` 호출의 `command`가 파괴적 형태 목록에 걸림 | `RequireApproval` | `shell:<program>` | **`false`** |
| 그 외 `shell` 호출 | `Allow` | — | — |
| `annotations.destructive` 이면서 워크스페이스로 한정되는 경로 인자가 **없음** | `RequireApproval` | `<tool>` | `true` |
| 그 외 | `Allow` | — | — |

파괴적 형태 목록(설정으로 덮어쓸 수 있고 기본값이 이것이다):
`rm -rf` · `rm -r -f` · `git push` · `git reset --hard` · `git clean -fd` · `sudo` ·
`chmod -R` · `chown -R` · `dd ` · `mkfs` · `shutdown` · `reboot` · `curl … | sh` ·
`wget … | sh` · `> /dev/sd*`.

네 번째 줄이 `annotations`를 쓰는 자리다. `write_file{path}`는 경로 인자가 워크스페이스
안으로 풀리므로 통과하고(그리고 `developer`에서는 `default.grant`도 통과한다),
`git_commit`(경로 인자 없음, `destructive: true`)은 걸린다.
**매처가 경계가 아니라는 것**을 plugin의 모듈 doc과 `security.md`에 적는다: 적대적으로 쓴
명령은 빠져나가고, 이 목록이 잡는 것은 모델의 실수다. 경계는 프로파일(§3.8)과 로그다.

**이 표에서 기억될 수 있는 승인은 1번과 4번 줄뿐이다.** 셸 키(`shell:<program>`)는 언제나
`allow_remember: false`와 함께 나오므로 이 빌드에서 `shell:cargo` 같은 기억은 **생기지
않는다.** 그래도 키를 프로그램 단위로 두는 이유는 로그다 — "이 세션에서 `rm` 게이트가 몇 번
걸렸나"가 `scope_key`로 묶여야 답이 된다(§2.2). 그리고 나중에 "이 세션 동안 `cargo`는 묻지
마라"를 열고 싶어지면 바꿀 것은 이 표의 한 칸이지 키의 모양이 아니다.

### 3.4 `plugins/sandbox-local` (4.5)

매니페스트: `capabilities = ["sandbox"]`, `permissions = [process_spawn, fs_read(workspace)]`.
`ProcessSpawn`이 `effective`에 없으면 등록하지 않는다 — `production`에서 이 plugin은 0개를
등록하고, 그 프로파일에서는 어떤 도구도 프로세스를 띄울 수 없다는 사실이 `plugin show`에
드러난다.

- `guarantees()` → 네 필드 전부 `false`. `security.md` §4의 표 그대로다.
- `prepare(request)` → `request.permissions`에 `ProcessSpawn`이 없으면 `Err(PolicyDenied)`.
  있으면 `LocalHandle`.
- `exec(spec, cancel)`:
  1. `cwd`를 `fsguard::resolve_dir(&workspace, cwd.unwrap_or("."))`로 푼다. **여기가 Phase 4의
     심볼릭 링크 게이트다** — 워크스페이스 안에서 밖을 가리키는 디렉터리 링크를 `cwd`로 주면
     canonicalize 뒤 재검사가 거부한다.
  2. `Command::new(spec.program).env_clear()` → `env_passthrough` 이름들을 호스트 환경에서
     읽어 넣고 → `spec.env`를 덮어쓴다. 이름만, 값은 로그에 안 남는다.
  3. unix에서 `process_group(0)` — 안전한 `std::os::unix::process::CommandExt` 메서드다.
     자식이 자기 프로세스 그룹의 리더가 되고, 그 그룹이 나중에 죽일 단위다.
  4. stdout/stderr를 동시에 읽되 `max_output_bytes`를 넘으면 **버리면서 계속 읽는다.**
     읽기를 멈추면 파이프가 차서 자식이 write에서 블록되고, 그 자식은 타임아웃으로만 끝난다.
     `truncated: true`로 신고한다.
  5. `spec.timeout_ms` · `cancel` 중 먼저 오는 것에 반응해 **그룹에** `SIGTERM`, 짧은 유예 뒤
     `SIGKILL`. 그 뒤 자식을 reap한다. `exit_code: None`, `timed_out`을 맞게 채운다.
- `teardown()` → 아직 살아 있는 그룹을 같은 절차로 죽인다. 멱등.

**시그널을 부르는 방법.** 워크스페이스가 `unsafe_code = "forbid"`이므로 `libc::killpg`를
직접 부를 수 없다. 안전 래퍼를 쓴다: `rustix`의 `kill_process_group`. `rustix 1.1.4`는 이미
`Cargo.lock`에 있으므로(다른 crate의 전이 의존) 빌드 비용이 사실상 0이고, `nix`를 새로
들이는 것보다 트리가 덜 늘어난다. **`[workspace.dependencies]`에 `rustix = { version = "1",
features = ["process"] }` 한 줄을 더하고** plugin은 `rustix.workspace = true`로 받는다 —
이 워크스페이스의 외부 의존은 예외 없이 그 표를 거친다. plugin 쪽에서는
`[target.'cfg(unix)'.dependencies]`에 넣는다, `rivet-runtime`이 `libc`를 그렇게 넣은 것과
같은 모양이다. **Windows는 프로세스 그룹 종료가 없고, 그래서 이 provider는 windows에서
자식만 죽인다.** 테스트도 `#[cfg(unix)]`다. `fsguard`가 windows에 대해 같은 문장을 이미
적어 뒀고, 여기서도 같게 적는다.

**새 의존 하나가 더 생긴다: `rivet-runtime`.** `cwd`를 `fsguard`로 풀어야 하므로
`tool-filesystem`이 이미 갖고 있는 그 edge를 이 crate도 갖는다(`plugins → rivet-runtime`,
역방향이 없으므로 순환은 아니다). `tool-git`도 같은 이유로 같은 edge를 갖는다 (§3.6).

### 3.5 `plugins/tool-shell` (4.6)

매니페스트: `capabilities = ["tool"]`,
`permissions = [process_spawn, fs_read(workspace), fs_write(workspace)]`.
`fs_write`를 **요청하는 것이 정직하다** — 임의 명령을 돌리는 도구는 파일을 쓸 수 있다.
등록 조건: `ProcessSpawn` **그리고** `FsWrite(Workspace)`가 둘 다 살아남았을 때만 `shell`을
등록한다. 그래서 `developer`·`ci`만 셸을 받는다.

- 입력: `{ command: string, cwd?: string, timeout_ms?: integer }`, `additionalProperties: false`.
- `ExecSpec::new("sh", ["-c", command])` — 셸이 필요하면 호출자가 명시한다는 계약 그대로이고,
  주입 표면이 argv로 세션 로그에 드러난다.
- `ctx.host.exec`로만 띄운다. 직접 spawn하지 않는다.
- `timeout_ms`는 `ctx.data.timeout_ms`와 **작은 쪽**을 쓴다. 도구가 한도를 넓힐 수 없다.
- 출력: `stdout`/`stderr`/`exit_code`를 사람이 읽는 형태로 조립하고, `exit_code != 0`이면
  `is_error: true`(실행 실패가 아니라 모델이 반응할 실패다). 비-UTF-8이 있었으면
  `structured = { "lossy": true }`. [`§11-7`](../architecture.md)에 대한 답이고,
  §2.2가 그 이유를 적는다.

### 3.6 `plugins/tool-git` (4.7)

매니페스트: `capabilities = ["tool"]`,
`permissions = [process_spawn, fs_read(workspace), fs_write(workspace)]`.

| 도구 | 등록 조건 | annotations |
|---|---|---|
| `git_status` | `ProcessSpawn` | `read_only` |
| `git_diff` | `ProcessSpawn` | `read_only` |
| `git_log` | `ProcessSpawn` | `read_only` |
| `git_commit` | `ProcessSpawn` **그리고** `FsWrite(Workspace)` | `destructive` |

권한 어휘가 "읽기 명령만"을 표현하지 못하므로 **도구가 그 구분을 진다.** `reviewer`는
`process_spawn`을 받고 `fs_write`를 못 받으므로 읽기 세 개만 얻는다 — 그게
[`security.md` §8](../security.md)의 "읽기 명령만"이다.

- 전부 `ctx.host.exec`로 `git`을 띄운다. `--no-pager`를 붙이고 `-c core.pager=cat`은 쓰지
  않는다(설정 주입은 그 자체로 `.git/config`가 deny 목록에 있는 이유다).
- 경로 인자(`path`)는 `fsguard::resolve_dir`/`resolve_file_parent`로 풀어 **워크스페이스 상대
  경로로 정규화한 뒤** argv에 넣는다. 링크로 밖을 가리키는 경로는 여기서 거부된다.
  그래서 이 crate도 `rivet-runtime` 의존을 갖는다 — `tool-filesystem`·`sandbox-local`과
  같은 edge이고 같은 이유다.
- `git_commit`은 `message`를 argv로 넘긴다(`-m`). `--author` 같은 임의 플래그는 받지 않는다 —
  입력 스키마가 닫혀 있다.
- 저장소가 아니면 `is_error: true`인 `ToolResult`로 돌려준다. `Err`가 아니다.

### 3.7 `crates/rivet-tui` + `crates/rivet-cli` — 승인 UI (4.4의 UI 절반)

**`rivet-tui`는 여전히 `rivet-core`만 본다.** `ApprovalSink`가 core의 계약이므로 `Tui`가
그것을 구현하는 것은 Phase 3의 규칙을 깨지 않는다 — `the_tui_crate_does_not_depend_on_the_runtime`은
그대로 통과한다.

**다만 이 crate의 다른 규칙에는 처음으로 예외가 생긴다.** `lib.rs`가 "이 crate는 **이벤트가
나르는 것만 보여준다**; 이벤트가 안 나르는 것이 필요하면 답은 import가 아니라 이벤트를 더하는
것"이라고 적었는데, `pending`은 이벤트가 아니라 `ApprovalSink::request`가 직접 채운다.
`every_panel_is_filled_from_events_alone`은 깨지지 않는다(그 테스트가 도는 패널들은 그대로
이벤트로만 채워진다) — 하지만 화면에 이벤트 아닌 출처가 하나 생긴 것은 사실이고, 그것을
`lib.rs`의 그 문단에 한 줄로 적는다. 예외인 이유: 승인은 관찰이 아니라 **왕복**이다. 버스는
lossy이고 단방향이므로 답을 돌려줄 수 없다. 이벤트로 표현했다면 `tool.approval.requested`를
보고 답을 어딘가로 되돌려야 하는데, 그 "어딘가"가 바로 `ApprovalSink`다.

**Phase 3이 이미 절반을 써 뒀다.** `AppState::apply`는 `ToolEvent::PolicyEvaluated` ·
`ApprovalRequested` · `ApprovalResolved` 세 변형을 **이미 처리한다** — 해당 tool 줄의
`progress`를 `"policy: <name>"` · `"awaiting approval: <reason>"` · `"approved"/"denied"`로
바꾸고, 그 자리의 주석이 "Phase 4 publishes these. Shown as a progress note rather than
dropped, so a policy decision is visible the day it starts being published"라고 적는다.
Phase 4가 하는 일은 그 세 토픽을 **실제로 발행하는 것**이고, 그러면 그 코드가 처음으로 돈다.
따라서 tool 줄 요약은 새로 만들 것이 없다. 부작용 하나를 적어 둔다: `tool.policy.evaluated`를
모든 호출에 발행하므로(§2.2) **모든 tool 줄이 정책 note를 달게 된다.** 위 주석이 의도한 바
그대로이고, 진행 메시지를 내는 도구는 그것을 덮어쓴다.

새로 만드는 것은 **상호작용**뿐이다.

- `AppState`에 `pending: Option<ApprovalView>` (`reason` · `preview` · `allow_remember`).
- `Tui::request`가 `pending`을 채우고 `oneshot::Receiver`를 await한다. `draw`가 그 위에 모달을
  그린다. 키: `y` = `Approved`, `a` = `ApprovedForSession`(`allow_remember`일 때만),
  `n`/`Esc` = `Denied`.
- **디스패처가 먼저 포기할 수 있다.** run이 취소되면 `Approvals::resolve`의 `select!`가
  이기고 receiver가 drop된다. 그러면 TUI의 `send`가 실패하고, 그 실패가 `pending`을 지우는
  신호다. 화면에 답 없는 모달이 남지 않는다.
- 승인 모달이 떠 있는 동안 `Intent::Cancel`(Ctrl-C)은 지금과 같이 run 취소로 간다 — 위 경로가
  모달을 정리한다. 승인 UI가 Ctrl-C를 먹지 않는다.

**`rivet-cli`의 `render/approve.rs` (신규)** — TUI가 아닌 모드의 sink.

- 프롬프트는 **stderr**로 나간다. stdout은 답이고, `rivet "..." > answer.md`가 프롬프트를
  삼키면 안 된다.
- 입력은 `spawn_blocking`으로 stdin 한 줄. `y` / `a` / `n`.
- **stdin이 터미널이 아니면 sink를 만들지 않는다.** 파이프에 물린 실행이 프롬프트에서
  매달리는 것이 정확히 DoD 1이 금지하는 것이고, `--headless`를 안 붙였다는 이유로 매달릴
  이유가 없다.
- 그래서 `run.rs`가 최종 `unattended`를 계산한다:
  `config.unattended || sink.is_none()`. `Config::unattended`는 "운영자가 그렇게 말했다"로
  남고, "사람이 답할 수 있는가"는 출력 모드와 tty를 아는 `run.rs`가 정한다. `rivet doctor`가
  둘을 구분해 출력한다.

### 3.8 `crates/rivet-cli` — 프로파일 5종·카탈로그·설정 (4.9)

- **`config.rs`의 `Profile::permissions()`** — `ProcessSpawn`을 `developer` · `ci` ·
  `readonly` · `reviewer`에 준다. `production`에는 주지 않는다.
  [`§11-10`](../architecture.md)의 세 미해결 permission 중 `ProcessSpawn`이 닫힌다.
  `SecretsRead`·`JobManage`는 요청자가 없으므로 그대로 둔다 (§7-3).
- **`Profile::tool_scope()`** — `Reviewer`에 `git_status`·`git_diff`·`git_log`를 더한다.
  `rivet.example.toml`의 `[agents.reviewer]`는 그중 **둘**을 적고 있다
  (`tools = ["read_file", "list_dir", "git_diff", "git_log"]`, `rivet.example.toml:98`) —
  즉 예제가 이미 요구하는 것이 `git_diff`·`git_log`이고, `git_status`는 그 둘과 같은
  등록 조건(`ProcessSpawn`만, `FsWrite` 없이 — §3.6의 표)을 가진 세 번째 읽기 명령이라
  같이 넣는다. 셋을 다르게 취급할 근거가 없고, 다르게 두면 프로파일 목록과 도구 표가
  갈라진다.
- **`Profile::approval_everything()` 같은 것은 만들지 않는다.** 그 판단은 `policy-default`의
  설정에 있다 (§3.3).
- **`FileConfig`의 `[sandbox]`가 살아난다** — `toml::Table`이던 것이
  `SandboxSection { provider: String }`(기본 `"local"`)이 되고, `Config.sandbox_provider:
  String`이 그 값을 나른다. `Inert`에서 `sandbox: bool` 필드가 **사라진다**(`Inert`는
  `job`·`named_agents` 둘만 남는다), `Config::load`의 `sandbox: !file.sandbox.is_empty()`
  줄도, `doctor`의 `if config.inert.sandbox { ... "no confinement is applied until Phase 4" }`
  블록도 함께 사라진다. 그 자리에는 해석된 provider 이름과 **그것이 실제로 등록되어 있는지**가
  출력된다.

  **`Inert.sandbox`가 붙들고 있던 약속과, 그것을 이제 무엇이 붙드는가.**
  그 필드가 존재한 이유는 `Inert`의 doc이 적어 뒀다 — "sandbox를 설정한 운영자는 그것이 아직
  아무것도 강제하지 않는다는 것을 들어야 한다". 그래서 그 필드가 나르던 사실은 두 개다:
  ① `[sandbox]` 섹션이 **파싱되어 어딘가로 실린다**(조용히 버려지지 않는다), ②
  그것이 **아직 강제되지 않는다**. Phase 4는 ②를 거짓으로 만들고, 그래서 ②를 말하던 필드도
  줄도 없어지는 것이 맞다. 남는 것은 ①이고, 그것을 붙드는 것은
  `Config.sandbox_provider`다 — `the_shipped_example_keeps_its_named_agent`의
  `assert!(config.inert.sandbox, "and the sandbox section is carried too")`가
  `assert_eq!(config.sandbox_provider, "local", "the sandbox section is carried, and now it
  resolves")`가 된다. 새 단언은 옛 것보다 **강하다**: 옛 것은 "섹션이 비어 있지 않았다"만
  말했고 `[sandbox]`에 아무 키나 있어도 참이었지만, 새 것은 예제가 실제로 적어 둔 provider
  이름이 무엇으로 해석되는지를 말한다. §6.0에 그 줄이 있다.
- **`doctor`의 판정은 "provider가 없다"가 아니라 "필요한데 없다"이다.** 출력은 언제나 하지만
  `healthy = false`는 아래 두 조건이 **모두** 참일 때만이다:

  > ① `host.loader.records()` 중 **`record.instance_id.is_some()`이고** `record.effective`에
  >    `ProcessSpawn`이 남은 것이 하나라도 있고,
  > ② `registry.sandbox(<해석된 이름>)`이 `None`이다.

  **`instance_id.is_some()`은 장식이 아니라 조건 ①의 전부다.** `catalog::load`는
  `discover(&sources())` 다음에 `validate()`를 **카탈로그 전체**에 돌리고
  ([`catalog.rs:143-145`](../../crates/rivet-cli/src/catalog.rs)), `validate`가 바로 그
  자리에서 `record.effective = manifest ∩ profile`을 계산한다
  ([`loader.rs:195`](../../crates/rivet-plugin/src/loader.rs)). 그래서 `enabled`가 빼 놓은
  `sandbox-local`·`tool-shell`·`tool-git`의 `effective`에도 `developer`에서는 `ProcessSpawn`이
  **남아 있다** — 로드되지 않았을 뿐이다. 필터 없이 "`effective`에 `ProcessSpawn`이 있는
  레코드"를 세면 하네스의 기본 설정
  (`enabled = ["rivet.model-openai", "rivet.tool-filesystem"]`,
  [`tests/support/mod.rs:201-202`](../../crates/rivet-cli/tests/support/mod.rs))에서도 조건 ①이
  참이 되고, §6.0이 "바뀌지 않는다"로 센 doctor 넷이 정확히 다시 exit 2가 된다. "로드된"은
  `instance_id`가 정하고, 그 판정은 이 파일에 이미 있다 —
  `report_plugins`가 `[plugins."<id>"]`의 고아 테이블을 찾을 때 쓰는 필터가 같은 것이다
  (`doctor.rs`의 `filter(|record| record.instance_id.is_some())`). 같은 뜻을 두 번째 방식으로
  적지 않는다.

  이 검사는 `report_plugins`가 아니라 `doctor::run`에 산다 — `Registry::sandbox`가 `async`이고
  `report_plugins`는 동기 함수다. 재료는 둘 다 `doctor::run`이 이미 들고 있는 `Host` 안에 있다.

  이유는 §2.1-a′와 같다. `healthy = false`는 `main.rs`에서 exit 2가 되고
  [`config.md`](../config.md)의 종료 코드 표는 2를 "파일이 잘못됐거나, 프로파일이 없거나,
  자격 증명이 없거나"로 정의한다. 조건 ①이 없으면 **`--profile production`이 어떤 설정으로도
  exit 0을 낼 수 없고**(그 프로파일은 `ProcessSpawn`을 주지 않으므로 `sandbox-local`이 0개를
  등록한다 — §4.5가 그 상태를 *의도된 것*이라고 적는다), `enabled`를 직접 적어 둔 기존
  `rivet.toml`은 업그레이드만으로 `rivet doctor`가 0에서 2로 바뀐다. 이 설계가 지키겠다고
  선언한 바로 그 두 집단이다. 조건 ①을 붙이면 셋 다 맞는다: `production`은 건강하고, 구형
  설정도 건강하며, `developer`에서 `tool-shell`을 켜 놓고 `sandbox-local`을 뺀 **진짜**
  오설정만 걸린다.

  **판정의 재료가 도구 이름이 아니라 `ProcessSpawn`인 것도 결정이다.** "프로세스를 띄우는
  도구가 등록되어 있는가"를 `shell`·`git_*` 같은 이름 목록으로 판단하면 호스트가 특정
  plugin의 도구 이름을 알게 된다 — Phase 2가 없앤 결합이고, `Config::api_key_env()`가 마지막
  부채로 남아 있는 바로 그 종류다([`§11-12`](../architecture.md)). `record.effective`는
  로더가 이미 들고 있고 어느 plugin에도 이름으로 묶이지 않는다.

  기존 세 `healthy = false` 조건(빈 deny 목록 · 자격 증명 부재 · plugin의 주장과 실제 등록의
  불일치)은 전부 "이 설정은 실제로 깨졌거나 보호되지 않는다"이다. 프로세스를 아무도 안 띄우는
  설정에 샌드박스가 없는 것은 그 셋 중 어느 것도 아니다.
- **`catalog.rs`** — 네 `PluginSource`를 더하고, `default_selection()`에 네 id를 전부 넣는다.
  `policy-default`가 기본에서 빠지면 기본 실행에 정책이 없고, 그건 Phase 4가 일어나지 않은
  것과 같다. `sandbox-local`이 빠지면 기본 provider `local`이 등록되지 않아 **프로세스를
  띄우는 호출만** 실패한다 — 나머지 도구는 그대로 돈다(§2.1-a′). 그래서 `enabled`를 직접
  적어 둔 기존 `rivet.toml`은 업그레이드 후에도 지금처럼 동작한다: 셸도 git도 그 목록에
  없으므로 등록되지 않고, 파일 도구는 프로세스를 띄우지 않는다. 셋과 넷(`shell`·`git`)은
  MVP 경계 표가 MVP 도구로 적어 둔 것이고, **누가 받는가는 선택이 아니라 프로파일이 정한다.**
기본 선택은 7개, 카탈로그는 8개가 된다 — `an_absent_enabled_list_selects_the_default_not_the_whole_catalog`가
단언하는 `chosen.len() < sources().len()`의 여유가 `telemetry-log` 하나로 줄어든다는 뜻이다.
줄어들 뿐 깨지지는 않고, 다음에 기본 밖 plugin이 하나 더 생기면 여유가 는다.
- **`run.rs`** — sink를 만들고, 최종 `unattended`를 계산하고, `RunConfig`에
  `sandbox_provider`를 싣는다.

### 3.9 문서·예제

| 파일 | 무엇을 |
|---|---|
| `docs/plan.md` | 진행 추적 표의 Phase 4 행을 ✅로. Phase 3 행의 서술 형식(테스트 수 · clippy · DoD · 설계 문서 링크 · 리뷰 반영)을 따른다. |
| `docs/architecture.md` | §11-6(시크릿) 범위 밖 판단, §11-7(비-UTF-8) **닫음**, **§11-9(provider egress vs tool egress) 의도적 재검토 기록** — 그 항목이 "Phase 4의 sandbox가 이걸 물려받기 전에 의도적으로 다시 열어야 한다"고 이름까지 적어 뒀다, §11-10의 `ProcessSpawn` **닫음**, §11-11(interceptor capability) **판단 기록**, §11-13(`fs_read` scope) 현황 갱신, §11-14(spec 고정) **닫음**, §11-15는 그대로 열려 있음을 재확인. |
| `docs/security.md` | §4의 `ExecSpec` 두 기본값에 `env_passthrough` 각주. §8 표의 `process` 열이 **처음으로 참이 됨**을 적고, `network` 열이 여전히 강제되지 않음(`sandbox-local`은 네트워크를 격리하지 않는다)을 남긴다. 파괴적 명령 매처가 경계가 아니라는 문단을 §6 옆에 새로 쓴다. |
| `docs/events.md` | "28개 중 20개 발행, 8개 대기"를 "23개 발행, 5개(`job.*`) 대기"로. `tool.execute.started.sandboxed`의 뜻을 한 줄로 정의한다. §6의 예제 코드가 `Option<PolicyDecision>` / `PolicyDecision::Deny`로 되어 있는데 실제 계약은 `Option<RestrictiveDecision>`이다 — Phase 4가 그 경로를 실제로 돌리므로 여기서 고친다. |
| `docs/plugin.md` | §1 표의 `Policy` 행에 interceptor가 같은 슬롯을 쓴다는 각주(§11-11 판단). §4.5b가 이제 동작한다는 것과, 샌드박스 없이 부르면 무슨 에러가 나는지. **그리고 도구 저자를 향한 새 문장 하나: "읽기 전용 도구는 `annotations.read_only = true`를 적어야 한다 — `ToolAnnotations::default()`는 `false`이고, Phase 4부터 `default.grant`가 그 기본값을 '변이한다'로 읽는다."** 이것이 §3.3의 두 번째 갈래이고, 안 적으면 Phase 5 이후의 도구 저자가 `readonly`에서 자기 읽기 도구가 막히는 것을 이유 없이 만난다. 정책이 파일시스템을 보지 않는다는 §5-24의 문장도 여기에 다시 적는다. |
| `docs/config.md` | `[sandbox]`를 "아직 동작하지 않는 섹션" 표에서 뺀다. `[plugins."rivet.sandbox-local"]`·`[plugins."rivet.policy-default"]` 레퍼런스. 프로파일 표에 `process`·`승인` 열 추가. `--headless`가 아니어도 stdin이 tty가 아니면 무인이 된다는 문장. **종료 코드 표의 `4 \| 정책 거부 (Phase 4부터 실제로 발생)` 줄을 고친다** — §7-5가 Phase 4에서도 exit 4를 만들지 않기로 정했으므로 이 줄은 그대로 두면 거짓이 된다. |
| `rivet.example.toml` | `enabled`가 둘에서 **여섯**이 된다: `rivet.model-openai` · `rivet.tool-filesystem` · `rivet.policy-default` · `rivet.sandbox-local` · `rivet.tool-shell` · `rivet.tool-git`. `:25-29`의 "Phase 4가 실을 때까지 빼 둔다" 주석은 지운다(그 문장이 이 Phase에 거짓이 된다). `:72-75`의 `[policy]` 앞 주석("정책 체인·승인·샌드박싱은 Phase 4에 온다")도 다시 쓴다. `[sandbox]` 주석 갱신, 새 plugin 테이블 둘(§4.4). **이 파일을 읽는 테스트가 셋이고 그중 둘이 깨진다 — §6.0.** |
| `Cargo.toml` (워크스페이스) | `[workspace.dependencies]`에 `rustix = { version = "1", features = ["process"] }` 한 줄. 이 저장소의 외부 의존은 예외 없이 여기를 거친다. |
| 네 plugin의 `Cargo.toml` | `rivet-core`·`async-trait`·`serde*`·`tokio`는 이미 있다. 더할 것: `sandbox-local`·`tool-git`에 `rivet-runtime`(fsguard), `sandbox-local`에 `rustix`(unix 한정)·`tracing`, **`policy-default`에 dev-dependency로 `rivet-runtime`**(§6.1 DoD 6의 테스트가 진짜 `Registry`와 `policy_chain::evaluate`를 지나야 하고, 화살 방향은 `plugin → rivet-runtime`으로 `tool-filesystem`과 같다 — 반대 방향은 만들지 않는다), 넷 다 dev-dependency로 `rivet-plugin`(자기 매니페스트를 파싱하는 `the shipped manifest parses` 테스트, `tool-filesystem/src/lib.rs:105`와 같은 모양)과 `tempfile`. |
| `examples/minimal-agent/src/main.rs` | `registry.tool("echo")`의 반환형이 `RegisteredTool`로 바뀌므로 `tool.spec().description` 한 줄을 고친다 (§3.2). 워크스페이스 멤버이므로 `clippy --all-targets`가 잡는다. |
| `crates/rivet-cli/tests/support/mod.rs` | **아무것도 더하지 않는다.** 리뷰 3이 짚은 대로 `Workspace::enable_plugins(&[&str])`가 이미 있고(`:228`) `e2e.rs:556`이 쓰고 있다 — `[plugins].enabled` 줄만 바꾸고 나머지 기본 설정은 그대로 두므로, 앞 라운드가 새로 만들려던 `with_plugins`가 하려던 일을 정확히 한다. 문이 둘이 되지 않는다. `--profile`·`--headless`는 `Workspace::run(&[..])`의 인자로 이미 넘길 수 있다. **`Workspace::new`가 쓰는 기본 설정 문자열은 손대지 않는다** — 그 문자열은 §6.0의 doctor 넷과 세션 이벤트 정확 비교가 서 있는 바닥이고, 게다가 `e2e.rs:337`의 `enable_telemetry`가 `enabled = ["rivet.model-openai", "rivet.tool-filesystem"]`이라는 **리터럴에 대해 문자열 치환**을 하므로, 그 줄이 바뀌면 그 헬퍼가 조용히 아무것도 안 하게 된다(테스트는 "telemetry가 로그를 안 남겼다"로 실패한다). |
| `crates/rivet-cli/src/doctor.rs` · `main.rs` · `config.rs` · `crates/rivet-runtime/src/dispatch.rs` | **사용자와 도구 저자가 읽는 "Phase 4" 문장 넷이 이 Phase가 끝나는 날 거짓이 된다.** `doctor.rs:35`의 출력 `"profile {} (narrows the agent's tool scope; policy enforcement is Phase 4)"`(운영자가 실제로 읽는 줄 — **`"  profile     "` 접두는 그대로 둔다**, `doctor_reports_what_the_configuration_resolved_to`가 `contains("profile     developer")`로 단언한다), `main.rs:57`의 `--profile` 도움말 "enforcement; that arrives in Phase 4", `config.rs:156`의 `Profile` doc "there is no policy chain until Phase 4", `dispatch.rs:66-69`의 `WORKSPACE_POLICY` doc "Phase 1 has no policy chain". 같은 편에 딸린 나머지 주석도 이번에 맞춘다: `dispatch.rs:7-10`의 파이프라인 표 4-7행과 `:19`·`:249`의 "empty but present", `dispatch.rs:553-560`의 거절 문구, `agent_loop.rs:88`, `lib.rs:14`, `config.rs:47`·`:255`·`:307`·`:445`, `render/mod.rs:30`, `exit.rs:29`(§7-5), `rivet-tui/src/state.rs:242`, `rivet-plugin/src/guard.rs:335`, 그리고 네 plugin crate의 모듈 doc 세 줄. |
| 기존 테스트 **다섯** (`event_flow.rs` 하나 · `config.rs` 셋 · `plugin_cmd.rs` 하나) | 기대값이나 픽스처가 바뀐다. 어느 것이, 왜, 그리고 그것이 붙들고 있던 약속을 그 뒤에 무엇이 붙드는지는 전부 §6.0의 표에 있다. |

---

## 4. 데이터·인터페이스 모양

### 4.1 매니페스트 네 개

```toml
# plugins/policy-default/rivet-plugin.toml
[plugin]
id           = "rivet.policy-default"
name         = "Default policy"
version      = "0.1.0"
abi_version  = "0.1"
description  = "Workspace containment and a destructive-command gate."
capabilities = ["policy"]
# 권한 없음. 정책은 PolicyRequest 의 순수 함수이고, 그것이 감사 시점에 로그만으로
# 결정을 재현할 수 있게 하는 조건이다.
```

```toml
# plugins/sandbox-local/rivet-plugin.toml
capabilities = ["sandbox"]
[[permissions]]
permission = "process_spawn"
[[permissions]]
permission = "fs_read"
scope      = "workspace"      # cwd 를 fsguard 로 풀기 위해 필요하다
```

```toml
# plugins/tool-shell/rivet-plugin.toml  (tool-git 도 같은 세 줄)
capabilities = ["tool"]
[[permissions]]
permission = "process_spawn"
[[permissions]]
permission = "fs_read"
scope      = "workspace"
[[permissions]]
permission = "fs_write"
scope      = "workspace"      # 임의 명령은 파일을 쓴다. 요청하지 않는 것이 거짓말이다.
```

### 4.2 `rivet-runtime`의 새 공개 API

```rust
/// 호스트가 fold 에 넣는 씨앗. 정책은 이것을 조일 수만 있다.
///
/// `constraints.sandbox` 는 **언제나 `None`** 이다. `merge` 의 그 축은 `self.or(other)`
/// 이고 씨앗은 언제나 `self` 이므로, 이름을 넣으면 provider 를 명시한 정책이 지고 만다 --
/// 다른 축과 정반대다. 설정의 provider 는 `DispatchCtx::sandbox_provider` 로 나른다.
#[derive(Clone, Debug, Default)]
pub struct Baseline {
    pub constraints: ExecutionConstraints,
}

/// 한 호출의 샌드박스. 디스패처와 도구 태스크가 `Arc` 로 함께 본다.
///
/// `teardown` 이 `&self` 인 것은 버려진 도구 태스크가 `Arc` 를 계속 잡고 있어 값을
/// 회수할 수 없기 때문이다. teardown 뒤에는 봉인되어 `exec` 가 `Err` 를 낸다 --
/// 그렇지 않으면 버려진 태스크가 내릴 주체 없는 컨테이너를 하나 더 띄운다.
#[derive(Debug)]
pub struct SandboxScope { /* name · provider · request · Mutex<ScopeState> */ }

impl SandboxScope {
    /// 이름을 정하고 레지스트리를 조회한다. **조회 실패는 에러가 아니다.**
    ///
    /// `request` 는 lazy prepare 가 그대로 쓸 `SandboxRequest` 다. `workspace` 는 이 호출의
    /// 워크스페이스, `permissions` 는 **정책이 좁힌 뒤의 grant**
    /// (`constraints.permissions` 가 있으면 그것, 없으면 `ctx.permissions`) -- 즉 도구가
    /// `ToolContextData.permissions` 로 받는 것과 같은 값이고, `sandbox-local::prepare` 가
    /// 거기서 `ProcessSpawn` 을 본다(§3.4). `options` 는 **Phase 4 에서 비어 있다**:
    /// provider 별 설정(`env_passthrough`)은 plugin 이 자기 `[plugins."<id>"]` 를 load 에서
    /// 읽으므로 호스트가 나를 것이 없다. 새 배선을 만들지 않는 쪽이다.
    pub async fn resolve(
        registry: &Registry,
        name: Option<&str>,
        request: SandboxRequest,
    ) -> Self;
    /// 이 호출에 등록된 샌드박스가 있는가. `tool.execute.started.sandboxed` 가 이 값이다.
    pub fn is_sandboxed(&self) -> bool;
    /// 첫 호출에서 prepare 한다. provider 가 없으면 이름을 대며 `Err(NotFound)`.
    pub async fn exec(&self, spec: ExecSpec, cancel: CancellationToken) -> Result<ExecOutput>;
    /// 멱등. 뒤이은 `exec` 는 `Err`.
    pub async fn teardown(&self);
}

/// 체인 전체. `spec` 이 인자인 것은 재작성 재평가가 3 단계(스키마 검증)를 다시 돌려야
/// 하는데 `PolicyRequest` 가 `input_schema` 를 나르지 않기 때문이다 -- 그 재료는 등록
/// 시점에 고정된 `RegisteredTool.spec` 에 있고, `dispatch` 가 이미 손에 들고 있다.
/// `cancel` 은 체인 전체와 `select!` 되는 토큰이다 (§2.1-b).
pub async fn evaluate(
    registry: &Registry,
    spec: &ToolSpec,
    request: PolicyRequest,
    baseline: Baseline,
    cancel: &CancellationToken,
) -> Evaluated;

/// 체인 한 번의 결과.
#[derive(Clone, Debug)]
pub struct Evaluated {
    pub decision: PolicyDecision,
    /// 최종 outcome 을 만든 정책/interceptor 의 이름. `tool.policy.evaluated.policy`
    /// 와 `ToolBlocked.policy` 에 같은 값이 들어간다. 아무도 결정하지 않았으면
    /// (정책 0 개, 또는 전부 `Allow`) 씨앗이 결정한 것이므로 `"host.baseline"` 이다.
    pub deciding: String,
    /// 재작성 재평가가 몇 바퀴 돌았는가. 0 이면 재작성이 없었다.
    pub rounds: u8,
}

/// interceptor 하나에 허용하는 시간. 초과하면 `None` 으로 접고 보고한다.
pub const INTERCEPTOR_TIMEOUT: Duration = Duration::from_millis(2_000);

/// 재작성이 수렴해야 하는 바퀴 수. 넘기면 Deny.
pub const REWRITE_DEPTH_LIMIT: u8 = 3;

/// 등록 시점에 고정된 spec 과 함께 돌려주는 도구.
#[derive(Clone, Debug)]
pub struct RegisteredTool {
    pub tool: Arc<dyn Tool>,
    pub spec: Arc<ToolSpec>,
}
```

`DispatchCtx`에 붙는 것:

```rust
    /// `[sandbox] provider`. 이 호출의 provider 는 `constraints.sandbox ∨ 이것` 이다.
    /// 해석 실패는 호출을 막지 않는다 -- 프로세스를 띄우려는 순간에만 실패한다.
    pub sandbox_provider: Option<String>,
    /// sink 와 기억된 승인. 기억은 세션 로그의 projection 이지 런타임 상태가 아니다.
    pub approvals: Approvals,
```

### 4.3 `shell`·`git`의 입력 스키마

```json
// shell
{ "type": "object",
  "properties": {
    "command":    { "type": "string",  "minLength": 1 },
    "cwd":        { "type": "string",  "minLength": 1 },
    "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000 } },
  "required": ["command"], "additionalProperties": false }

// git_diff
{ "type": "object",
  "properties": {
    "path":   { "type": "string", "minLength": 1 },
    "staged": { "type": "boolean" },
    "rev":    { "type": "string", "minLength": 1 } },
  "additionalProperties": false }

// git_commit
{ "type": "object",
  "properties": {
    "message": { "type": "string", "minLength": 1 },
    "all":     { "type": "boolean" } },
  "required": ["message"], "additionalProperties": false }
```

키워드는 전부 `schema::validate_spec`이 강제하는 15개 안에 있다. 그것을 벗어나면 등록이
실패하고, 그건 "강제되지 않는 제약을 광고하는 도구"를 막는 Phase 1의 성질이다.

### 4.4 설정 표면

```toml
[sandbox]
provider = "local"        # 이제 실제로 해석된다. 이 이름이 등록되어 있지 않으면
                          # **프로세스를 띄우는 호출만** 실패한다 -- 나머지 도구는 그대로
                          # 돈다. `rivet doctor` 가 run 전에 그 사실을 출력하고, 프로세스를
                          # 띄울 수 있는 plugin이 실제로 로드된 경우에만 exit 2를 낸다.

[plugins."rivet.sandbox-local"]
# 빈 환경에서 시작해 이 이름들만 호스트 환경에서 가져온다. 값이 아니라 이름을 적는다.
# 목록에 없는 것은 새지 않는다 -- AWS_SECRET_ACCESS_KEY 를 넣으려면 여기 타이핑해야 한다.
env_passthrough = ["PATH", "HOME", "LANG", "LC_ALL", "TZ"]
# TERM 은 기본값에 없다. 있으면 도구들이 ANSI 를 뱉고, 그건 모델이 읽을 텍스트가 아니다.

[plugins."rivet.policy-default"]
require_approval_for_all_in = ["production"]
# 기본 목록을 통째로 대체한다. 비우면 파괴적 형태 게이트가 꺼진다 -- doctor 가 그렇게 말한다.
# destructive_commands = ["rm -rf", "git push", ...]
```

### 4.5 프로파일 — 무엇이 실제로 바뀌는가

| 프로파일 | fs write | process | 등록되는 도구 | 승인 | 무인 |
|---|---|---|---|---|---|
| `developer` | ○ | ○ | filesystem 4 · `shell` · git 4 | 파괴적 형태만 | |
| `ci` | ○ | ○ | filesystem 4 · `shell` · git 4 | 불가 → 거부 | ○ |
| `readonly` | ✗ | ○ | filesystem 3 · git 3 | — | |
| `reviewer` | ✗ | ○ | filesystem 3 · git 3 (scope로도 한 번 더) | — | |
| `production` | ✗ | ✗ | filesystem 3 | **전부** | |

`readonly`가 `process ○`인 것이 [`security.md` §8](../security.md)의 "제한적"이다 —
프로세스는 띄울 수 있지만 셸은 못 받고, 받을 수 있는 것은 git 읽기 명령 셋이다.
`production`의 `process ✗`는 `sandbox-local`이 아예 등록되지 않는다는 뜻이고, 그래서 그
프로파일에서는 `[sandbox] provider`가 무엇이든 프로세스가 뜨지 않는다.

**그리고 그것이 `production`에서 `read_file`을 막지는 않는다.** 그 프로파일에 등록되는
도구 셋은 프로세스를 띄우지 않으므로 샌드박스가 없다는 사실을 만날 일이 없다 — 7단계는
`sandboxed: false`를 기록하고 지나간다(§2.1-a′). 이 표의 "등록되는 도구" 열은 실제로
**돌아가는** 도구를 말한다.

### 4.6 세션 로그에 남는 한 쌍

승인된 호출:

```text
tool.called        은 아직 아니다 -- 승인이 먼저다
approval.requested { call_id, reason: "destructive", preview: "rm -rf build",
                     scope_key: "shell:rm" }
approval.resolved  { call_id, scope_key: "shell:rm", outcome: "approved",
                     remembered: false, actor: "kangwoo" }
tool.called        { call: { name: "shell", input: { command: "rm -rf build" }}}
tool.completed     { call_id, duration_ms: 120 }
```

`--headless`에서 (DoD 1):

```text
approval.requested { call_id, reason: "destructive", preview: "rm -rf /", scope_key: "shell:rm" }
approval.resolved  { call_id, scope_key: "shell:rm", outcome: "denied",
                     remembered: false, actor: null }
tool.blocked       { call_id, policy: "default.destructive",
                     reason: "approval required, and nothing can ask a human" }
```

기억을 **만드는** 승인. 이 빌드에서 `allow_remember: true`가 나오는 자리는 두 곳뿐이고
(§3.3 표의 1·4번 줄), 아래는 4번 줄인 `git_commit`이다 — 셸 키는 언제나
`allow_remember: false`이므로 `shell:*`은 기억되지 않는다:

```text
approval.requested { call_id, reason: "destructive", preview: "git commit -m \"wip\"",
                     scope_key: "git_commit" }
approval.resolved  { call_id, scope_key: "git_commit", outcome: "approved_for_session",
                     remembered: true, actor: "kangwoo" }
tool.called        { ... }
```

resume 후, 같은 `scope_key`의 두 번째 호출 (DoD 3):

```text
approval.requested { ... scope_key: "git_commit" }
approval.resolved  { ... outcome: "approved", remembered: false, actor: null }
tool.called        { ... }
```

마지막 블록의 `remembered: false`가 중요하다. `SessionState::apply`는
`remembered && outcome == ApprovedForSession`일 때만 집합에 넣고 **중복을 접지 않으므로**,
`true`로 다시 쓰면 같은 키가 누적된다. 기억을 **만든** 이벤트는 하나뿐이고, 나머지는 그것을
쓴 기록이다.

---

## 5. 실패 모드

| # | 실패 | 처리 |
|---|---|---|
| 1 | `--headless`에서 승인이 필요하다 | 6단계의 ②가 `RequireApproval` → `Deny`로 바꾼다. 매달리지 않는다. 쌍은 남고 `actor: null`이다 (DoD 1). |
| 2 | `--headless`가 아닌데 stdin이 파이프다 | sink를 만들지 않고 `unattended`가 참이 된다 (§3.7). 프롬프트에서 매달리는 경로가 존재하지 않는다. |
| 3 | 승인을 기다리는 동안 run 데드라인이 지난다 | `Approvals::resolve`의 `select!`가 `ctx.cancel`로 깨어나 `TimedOut`으로 해소한다. 별도 승인 타임아웃은 발명하지 않는다 — run 한도가 이미 여기 도달한다. |
| 4 | 승인을 기다리는 동안 Ctrl-C | §3과 같은 경로. disposition은 `Blocked`가 아니라 "시작되지 않았다" 문구의 `Completed`다. 호출은 증명 가능하게 아무 효과도 없었고, `Blocked`는 연속 오류 카운터를 올린다. `ApprovalOutcome::TimedOut`의 계약 doc이 "Treated as `Denied`"라고 적은 것과 어긋나지 않는다 — 그 문장이 정하는 것은 **호출이 진행되지 않는다**이고, 진행되지 않는다. 갈라지는 것은 그다음, 즉 모델에게 뭐라고 말하고 카운터를 올리느냐이며 그건 그 doc이 다루는 범위가 아니다. 로그는 둘을 `outcome`으로 구분한다: 사람이 거절한 것은 `denied`, 기다림을 그만둔 것은 `timed_out`. |
| 5 | interceptor가 멈춘다 | 2 s 뒤 `None`으로 접고 `tracing::warn!`. 계약이 정한 처리다. 나머지 interceptor는 동시에 돌고 있었으므로 영향받지 않는다. |
| 6 | interceptor가 `Err`를 돌려준다 | `None`과 같이 취급하고 보고한다. `Err`를 `Deny`로 올리면 버그 하나가 런타임을 멈추고, `Allow`로 내리면 확장점이 조용히 사라진다. 기권이 둘 사이의 정직한 답이다. |
| 7 | 두 정책이 서로 다른 rewrite를 요구한다 | `combine`이 이미 `Deny`로 만든다. 런타임은 그 결정을 나를 뿐이다. |
| 8 | rewrite가 호출의 정체성(id·이름)을 바꾼다 | `rewrite_is_wellformed` 실패 → 즉시 `Deny`. 그 rewrite는 이 호출의 승인을 걸친 다른 호출이다. |
| 9 | 정책들이 rewrite로 수렴하지 않는다 | 3바퀴 뒤 `Deny`. 무한 루프가 아니라 거부다. 바퀴 수는 `Evaluated.rounds`로 나가고 사유에 적힌다. |
| 9b | 재작성 라운드가 interceptor 타임아웃을 곱한다 | 라운드당 2 s이므로 호출당 최악 6 s다. **별도 예산을 발명하지 않는다** — 체인 전체를 `ctx.cancel`과 `select!`하고, 취소·run 데드라인이 오면 호출을 "시작되지 않았다"로 닫는다(§5-4와 같은 경로). 6 s는 `REWRITE_DEPTH_LIMIT × INTERCEPTOR_TIMEOUT`에서 **유도된** 상한이고, 이 빌드는 interceptor를 하나도 싣지 않으므로 실측 상한은 0이다 (§2.1-b). |
| 10 | provider 이름이 등록되지 않은 것을 가리킨다 | 7단계는 **막지 않는다.** `sandboxed: false`로 기록하고 지나가며, `ToolHost::exec`가 불리는 순간 이름을 대며 `Err(NotFound)`가 된다. 7단계에서 막으면 `production`의 `read_file`과, `enabled`를 직접 적어 둔 기존 설정의 **모든** 호출이 함께 막힌다 (§2.1-a′). `rivet doctor`가 run 전에 같은 조회를 하고 **출력한다** — 다만 `healthy = false`로 만드는 것은 `ProcessSpawn`이 살아 있는 plugin이 실제로 로드된 경우뿐이다. 같은 실패를 판정으로 옮기면 7단계에서 막는 것과 같은 두 집단이 이번엔 exit 2를 맞는다 (§3.8). |
| 10b | teardown 뒤에 버려진 도구 태스크가 `exec`를 다시 부른다 | scope가 봉인되어 있으므로 `Err`. 봉인이 없으면 lazy prepare가 **새 handle**을 만들고 그것을 내릴 주체는 이미 사라진 뒤다 — `local`에서는 자식 토큰이 이미 취소돼 있어 새 자식이 곧 죽지만, `docker`에서는 컨테이너가 남는다 (§2.1-e). |
| 11 | 두 정책이 **서로 다른 provider**를 요구한다 | `ExecutionConstraints::merge`가 `self.or(other)`이므로 왼쪽이 남는다 — rewrite와 달리 거부하지 않는다. 이 빌드에는 provider가 하나뿐이라 관측될 수 없다. `rivet-core` 변경이므로 이번에 고치지 않고 §7-2에 적는다. |
| 12 | 취소·타임아웃 시 자식 프로세스가 남는다 | 프로세스 **그룹**에 `SIGTERM` → 유예 → `SIGKILL`. scope를 디스패처가 `Arc`로 들고 있으므로 도구 태스크를 버려도 죽일 주체가 남는다 (§2.1-e, DoD 4). |
| 13 | 8·9단계가 패닉한다 | `catch_unwind` 뒤에 `teardown()`이 있다. 패닉은 기존 경로대로 `Panicked` disposition이 된다. |
| 14 | 프로파일이 plugin의 권한을 전부 깎아 0개를 등록한다 | 로드는 성공하고 `registered`가 빈다(`production`의 `tool-shell`·`tool-git`·`sandbox-local`). 스텁이 아니라 프로파일의 결과이고, `rivet plugin show`가 어느 권한이 깎였는지 말한다. Phase 2가 거절한 "아무것도 등록하지 않는 plugin"은 **어떤 프로파일에서도** 등록하지 않는 것이었다. |
| 15 | 셸 명령이 파괴적 매처를 빠져나간다 | 빠져나간다. 매처는 경계가 아니다 (§2.1-f). 경계는 ① 셸을 받는 프로파일이 둘뿐이고 ② 그 둘은 이미 쓰기 권한이 있으며 ③ argv가 세션 로그에 남는다는 것이다. 문서에 그렇게 적는다. |
| 16 | 셸이 워크스페이스 밖 파일을 읽는다 | `sandbox-local`은 `filesystem_isolation: false`를 신고한다. 막지 못하고, 막는 척하지 않는다. 막으려면 `docker`/`podman` provider가 필요하고 그건 이 Phase의 범위 밖이다. |
| 17 | `cwd`가 워크스페이스 밖을 가리키는 링크다 | `fsguard::resolve_dir`가 canonicalize 뒤 재검사해서 거부한다. 진행 추적 표의 Phase 4 게이트가 여기다. |
| 18 | 자식이 출력을 무한히 쏟는다 | `max_output_bytes`까지만 보관하고 그 뒤로는 **읽으면서 버린다.** 읽기를 멈추면 파이프가 차서 자식이 블록되고, 그 자식은 타임아웃으로만 끝난다. |
| 19 | 자식 출력이 UTF-8이 아니다 | lossy 변환. `structured.lossy = true`로 신고한다. 조용히 치환하지 않는다. |
| 20 | 빈 환경 때문에 `cargo`를 못 찾는다 | `env_passthrough` 기본값에 `PATH`·`HOME`이 있다. 이름 목록이지 상속이 아니고, `rivet doctor`가 해석된 이름들을(값이 아니라) 출력한다. |
| 21 | 운영자가 `env_passthrough`에 비밀을 적는다 | 적히는 대로 전달한다. 두 번 판단하지 않는다 — "누군가 그것을 명시적으로 적어야 한다"가 계약이고, 여기가 그 "적는 자리"다. 대신 `doctor`가 이름을 보여줘서 보이지 않게 새지는 않는다. |
| 22 | 호스트 프로세스가 `kill -9` 당한다 | 자식 그룹이 남는다. 어떤 런타임 코드도 이걸 막을 수 없다. `plugin.md`와 `security.md`에 한 줄로 적는다. |
| 23 | 승인 모달이 떠 있는데 run이 끝난다 | receiver가 drop되고 TUI의 `send`가 실패해 `pending`이 지워진다. 답 없는 모달이 남지 않는다 (§3.7). |
| 24 | 정책이 파일시스템을 본다 | 계약 위반이지만 타입이 막지 못한다. 이 빌드가 싣는 세 정책은 보지 않고, `PolicyRequest`에 파일시스템 핸들이 없다는 것이 유일한 강제다. 감사 재현성이 걸린 문제이므로 `plugin.md`에 다시 적는다. |
| 25 | 도구가 `read_only: true`라고 거짓말한다 | `default.grant`를 지난다. annotations는 자기 신고이고, [`security.md` §5](../security.md)가 in-process plugin을 신뢰된 코드로 못박았으므로 이 층이 막는 것은 실수이지 악의가 아니다 (§2.1-g). 진짜 답은 `ToolSpec`이 요구 permission을 선언하는 것이고 §7-14에 있다. |
| 25b | 도구가 annotations를 **아예 안 적었다** | `ToolAnnotations::default()`가 `read_only: false`이므로 `default.grant`가 **막는다** — `readonly`·`reviewer`·`production`에서 그 도구는 읽기 전용이더라도 돌지 않는다. 25의 반대 방향이고, 이쪽이 실제로 더 자주 부딪힌다(거짓말은 드물고 빠뜨림은 흔하다). fail-closed이므로 방향은 맞지만 **조용하면 안 된다**: 거부 사유가 "이 도구는 `read_only`를 선언하지 않았다"를 명시하고, `plugin.md`가 도구 저자에게 "읽기 전용 도구는 `read_only: true`를 적어야 한다"를 말한다 (§3.3, §3.9). 이 빌드의 읽기 도구 셋은 전부 적어 뒀다. |
| 26 | `--headless`로 resume했는데 기억된 승인이 있다 | 기억이 무인 변환보다 **앞**이므로 그 승인은 유효하다 (§2.1-c). 사람이 durable하게 남긴 결정을 현재의 플래그가 지우지 않는다. 뒤집으면 더 닫히는 쪽이고, 그 선택지를 §7-13에 적어 둔다. |

### 알려진 미해결 — `Plugin::load`/`unload`에 여전히 데드라인이 없다

[`architecture.md` §11-15](../architecture.md)가 명명한 위험이고, Phase 2도 Phase 3도 고치지
않았으며 **Phase 4도 고치지 않는다.** 과제 서술이 "4.2가 `Interceptor` 타임아웃을 도입하므로
같은 모양을 재사용할지 판단하라"고 물었으므로 판단을 적는다.

**같은 모양이 아니다.** interceptor의 타임아웃에 대해 열려 있던 것은 **숫자뿐**이었다 —
초과했을 때 무엇을 남기는가는 계약이 이미 적어 뒀다("멈춘 interceptor는 `None`으로 처리되고
보고된다"). 그래서 숫자를 발명해도 대가가 정해져 있고, 그 대가는 기권 하나다.
`Plugin::load`에 대해 열려 있는 것은 **초과했을 때 남길 상태**이고(`FAILED` 레코드인가,
호스트 중단인가), 그건 "닿지 않는 상대"가 예외가 아니라 일상이 되는 Phase 6의 결정이다.
설계가 지정하지 않은 예산은 그 자체로 새 실패 모드다 — 부하 걸린 CI에서 느린 `load`가
`FAILED`가 된다. 판단 기준은 Phase 3이 drain 예산 2 s를 발명해도 된다고 본 것과 같다:
**초과의 대가가 계약에 이미 쓰여 있으면 숫자는 발명해도 되고, 쓰여 있지 않으면 안 된다.**

**Phase 4는 이 위험을 늘린다** — 네 plugin이 늘고, 그중 `sandbox-local`의 `load`는 설정을
읽고 하나를 등록할 뿐이지만 `policy-default`·`tool-shell`·`tool-git`도 마찬가지로 기다리는
것이 없다. 넷 다 네트워크도 파일도 건드리지 않는다. 표면은 넷 늘고 §11-15가 그리는 형태
("닿지 않는 백엔드를 `load` 안에서 기다리는")는 여전히 이 저장소에 없다. 그 문장을
§11-15에 덧붙인다.

---

## 6. 테스트 전략

전부 offline (`cargo test --workspace --offline`).

**기준선은 579다, 559가 아니다.** 과제 서술의 완료 판정 C가 인용하는 559는 `docs/plan.md`가
적은 **Phase 3 종료 시점**의 수이고, 이 브랜치의 base인 `4c7bfae`에는 그 뒤의 다섯 커밋이
얹혀 있다. base에서 그대로 돌린 결과는 **579 passed · 0 failed · 1 ignored**이고, 유일한
ignored는 `plugins/model-openai/tests/live.rs`의 `live_round_trip`(실제 provider가 필요해
`RIVET_LIVE=1`로만 돈다)이다. Phase 4가 지켜야 할 수는 이 579이고, 559를 목표로 삼으면
스무 개가 사라져도 게이트가 통과한다. 유지하거나 올려야 할 기준선:
**579 passed · 0 failed · 1 ignored · clippy 0 · `cargo doc` 0**.

**사라지는 테스트는 없다** — 아래 §6.0이 손대는 다섯은 전부 기대값이나 픽스처가 바뀌는
것이지 없어지는 것이 아니다.

### 6.0 기존 테스트에 무엇이 일어나는가

이 절이 있는 이유, 그리고 이번 라운드에 다시 쓴 이유. 리뷰 2의 blocking은 "설계가 기존
테스트를 조용히 깨뜨린다"였고, 이 절은 그것을 막으려고 생겼다. 리뷰 3은 **같은 실패를 이
절 안에서** 찾았다 — 표가 `rivet.example.toml`을 읽는 테스트를 하나만 세고 같은 파일을 읽는
형제 둘을 지나쳤다. 그래서 이번에는 표를 고치는 것이 아니라 **훑는 방법을 바꾸고 표를 다시
만들었다.** 방법을 먼저 적는 이유는 다음 리뷰어가 표를 믿는 대신 **재현해서 반증**할 수
있어야 하기 때문이다.

#### 어떻게 훑었나 — 그대로 재현할 수 있는 세 갈래

전부 워크트리 `herdr/phase-4-policy-sandbox` (base `4c7bfae`)에서 돌렸다.

**① 바뀌는 심볼로.** 이 설계가 이름을 바꾸거나 없애거나 형을 바꾸는 것 하나하나를
워크스페이스 전체(`--include='*.rs' --include='*.toml'`, `target/` 제외)에서 찾았다:
`Inert` / `inert.` · `tool_scope` · `permissions()` · `ProcessSpawn` / `process_spawn` ·
`sources()` / `default_selection` · `.spec()` / `registry.tool(` / `RegisteredTool` ·
`DispatchCtx {` · `RunConfig` · `Approval` · `sandboxed` · `Registry::sandbox` ·
`ExecutionConstraints` · `ToolAnnotations`.

**② 단언되는 리터럴로.** 심볼을 하나도 지나가지 않는 단언이 있다 — 문자열을 비교하는
e2e가 그렇다. 그래서 **값으로도** 찾았다: `rivet.tool-shell` · `rivet.tool-git` ·
`rivet.policy-default` · `rivet.sandbox-local` · `read_file`/`list_dir`/`search` ·
`tool.policy.evaluated`/`tool.approval.*` · `Blocked by policy` · `Phase 4` ·
`[plugins].enabled` · `enabled = [`.

**③ 저장소의 실제 파일을 읽는 테스트로 — 이 갈래가 지난 라운드에 없었다.** 리뷰 3의
blocking 둘은 둘 다 `rivet.example.toml`을 읽는 테스트이고, ①과 ②로는 잡히지 않는다:
그 테스트들은 파일을 `std::fs::read_to_string`으로 읽으므로 바뀌는 것은 **파일의 내용**이지
어떤 심볼도 아니고, 단언하는 리터럴(`PluginSelection::Only([...])`)은 파일 이름과 아무
관계가 없다. 파일 이름으로 grep하면 그 파일을 **언급하는** 곳이 나올 뿐이고, 지난 표는
그렇게 세어서 셋 중 하나만 잡았다.

그래서 파일 이름이 아니라 **읽는 행위**로 찾는다:

```bash
grep -rn "read_to_string\|include_str!" --include='*.rs' . | grep -v '^./target'
```

결과에서 tempdir가 만든 파일을 읽는 것(하네스·fsguard·scaffold 검증)을 걷어내면,
**저장소에 실제로 존재하는 파일을 읽는 테스트는 정확히 넷**이다:

| 읽는 파일 | 읽는 테스트 | 이 설계가 그 파일을 바꾸는가 |
|---|---|---|
| `rivet.example.toml` | `the_shipped_example_loads` (`rivet-cli/src/config.rs:738`) | **바꾼다 → 깨진다** |
| `rivet.example.toml` | `the_shipped_example_keeps_its_named_agent` (`config.rs:764`) | **바꾼다 → 컴파일도 안 된다** |
| `rivet.example.toml` | `every_id_the_shipped_example_enables_resolves` (`catalog.rs:327`) | 바꾸지만 통과한다 |
| `crates/rivet-tui/Cargo.toml` | `the_tui_crate_does_not_depend_on_the_runtime` (`tui/tests/independence.rs:19`) | 바꾸지 않는다 |

네 번째 줄까지 적는 이유는, 이 방법이 "예제 파일"이라는 한 사례가 아니라 **부류 전체**를
훑었다는 것이 그 줄로 확인되기 때문이다. 그리고 넷째는 §3.7이 `rivet-tui`를 `rivet-core`
안에 묶어 두겠다고 한 결정이 실제로 테스트에 걸려 있다는 확인이기도 하다.

같은 부류에 하나 더 있다: **각 plugin이 `include_str!("../rivet-plugin.toml")`로 자기
매니페스트를 컴파일에 끌어들이고**, 그 옆에 `the shipped manifest parses` 단위 테스트가
붙어 있다(`tool-filesystem/src/lib.rs:36`·`:105` 등 네 곳). §4.1이 만드는 매니페스트 넷도
같은 모양을 따르므로, 그 넷은 **새 테스트가 아니라 기존 관례가 자동으로 덮는 표면**이다.

#### 표 — 이 Phase가 건드리는 기존 단언 전부

두 부분이다. 먼저 **깨지는 다섯**(각각 그 단언이 붙들고 있던 약속과, 그 뒤에 무엇이 그
약속을 붙드는지를 적는다 — 리뷰 3이 요구한 형식이다), 그다음 **바뀔 법한데 안 바뀌는
것들**(왜 안 바뀌는지가 근거이지 희망이 아니어야 한다).

##### A. 깨진다 — 다섯

| # | 기존 단언 | 무엇이 깨지는가 / 그 단언이 붙들던 약속 / 그 뒤에 무엇이 붙드는가 |
|---|---|---|
| A1 | `every_bus_topic_is_claimed` (`rivet-runtime/tests/event_flow.rs:73`), 두 목록 `:126-165` | **기대값.** 세 `ToolEvent` 변형이 `Owner::Deferred(4)` → `Owner::Published`로 옮겨진다. 편집 위치가 정확히 어디인지가 중요하다: `ToolEvent::one_of_each()`의 변형 순서는 `Requested · PolicyEvaluated · ApprovalRequested · ApprovalResolved · Started · Progress · Completed · Blocked`이므로(`rivet-core/src/event.rs:289-327`), 세 토픽은 `published` 목록의 **끝에 붙는 것이 아니라** `"tool.requested"` 바로 다음에 끼어든다 — `tool.requested` · `tool.policy.evaluated` · `tool.approval.requested` · `tool.approval.resolved` · `tool.execute.started` · … 순이다. `deferred`는 `job.*` 다섯만 남고 메시지는 `"eight topics wait on Phase 4 (3) and Phase 5 (5)"` → `"five topics wait on Phase 5"`. **약속**: 모든 토픽에 발행자가 있거나, 없다면 어느 Phase가 채우는지 이름이 붙어 있다. **그 뒤에 붙드는 것**: 같은 테스트다 — 이건 기대값이 바뀌는 것이지 성질이 사라지는 것이 아니고, 이 편집을 **강제하는** 것이 과제 서술이 말한 Phase 3의 컴파일 타임 tripwire다. 세 토픽이 실제로 발행되는지는 §6.3의 `the_bus_carries_both_approval_topics`와 `every_call_publishes_a_policy_decision`이 따로 본다. |
| A2 | `profiles_narrow_tool_scope_and_permissions` (`rivet-cli/src/config.rs:946`, 단언 `:950-954`) | **기대값.** `Profile::Reviewer.tool_scope()`를 `["read_file","list_dir","search"]`와 **정확 비교**하는데 §3.8이 `git_status`·`git_diff`·`git_log`를 더한다. 목록을 여섯으로 고친다. 같은 테스트의 `permissions()` 단언 셋은 `allows(FsRead)`·`!allows(FsWrite)`·`allows(NetworkHttp)`뿐이므로 `ProcessSpawn` 추가로는 깨지지 않는다. **약속**: 그 자리의 코멘트가 적은 그대로 — "코드를 고칠 수 있는 reviewer는 reviewer가 아니다". **그 뒤에 붙드는 것**: 같은 코멘트, 그리고 그것이 계속 참인 이유 — 더해지는 셋은 전부 읽기 명령이고 `git_commit`은 **들어가지 않는다**. 코멘트는 그대로 두고, 그 문장이 여전히 성립한다는 것을 §6.3의 `a_reviewer_can_read_the_diff_but_not_commit`이 별도로 단언한다. |
| A3 | `the_shipped_example_loads` (`config.rs:734`, 단언 `:750-756`) | **기대값.** `config.plugins`를 `PluginSelection::Only([rivet.model-openai, rivet.tool-filesystem])`와 **정확 비교**하는데 §3.9가 예제의 `enabled`를 여섯으로 만든다. 목록을 여섯으로 고친다(순서는 파일에 적힌 순서 그대로 — 이 비교는 `Vec` 비교이고 `Config::load`는 `enabled`를 최초 등장 순으로 dedup만 한다). **약속**: 이 단언이 실제로 붙들던 것은 "예제가 켜는 것은 정확히 이 둘"이라는 **내용**이라기보다, 그 옆의 다섯 단언(model · profile · limits · deny · api_key_env)과 같은 종류의 **파싱** 약속이다 — *예제의 `[plugins].enabled`는 적힌 그대로 실린다; 호스트가 몰래 더하지도 빼지도 않는다.* 그 약속은 목록이 여섯이 되어도 그대로다. **그 뒤에 붙드는 것**: (ⅰ) 같은 단언, 목록만 여섯. (ⅱ) 그리고 여섯이 **왜 그 여섯인지**는 리터럴 하나로는 더 이상 자명하지 않으므로, 관계를 단언하는 줄을 이 테스트에 한 줄 더한다 — `Phase 4 이후 예제의 enabled ∪ {context-builtin} == catalog::default_selection()`. 그 등식이 참인 이유는 §3.8이 넷을 기본 선택에 넣고 §3.9가 같은 넷을 예제에 넣기 때문이고, 예제가 스스로 "기본 선택과 다른 것은 `telemetry-log` 하나뿐"이라고 적고 있다(`rivet.example.toml:31-35`). 리터럴을 베껴 두는 대신 그것이 서 있던 불변식을 적는 것이고, 다음 Phase가 한쪽에만 plugin을 더하면 이 줄이 먼저 실패한다. |
| A4 | `the_shipped_example_keeps_its_named_agent` (`config.rs:760`, 단언 `:771-774`) | **컴파일되지 않는다.** `assert!(config.inert.sandbox, "and the sandbox section is carried too")`인데 §3.8이 `Inert.sandbox` 필드를 없앤다. 실행 이전에 멈추므로 `cargo clippy --workspace --all-targets -- -D warnings`와 `cargo test --workspace`가 **둘 다** 여기서 끝난다. **약속**: `Inert`의 doc이 적은 그대로 두 가지다 — ① `[sandbox]` 섹션이 파싱되어 어딘가로 실린다(조용히 버려지지 않는다), ② 그것이 아직 아무것도 강제하지 않는다. **그 뒤에 붙드는 것**: ②는 Phase 4가 거짓으로 만드는 사실이므로 그것을 말하던 필드도 `doctor`의 그 줄도 함께 없어지는 것이 맞다. ①은 남고, 그것을 `Config.sandbox_provider`가 붙든다 — 단언이 `assert_eq!(config.sandbox_provider, "local")`이 된다. 새 단언이 옛것보다 **강하다**: 옛것은 "섹션이 비어 있지 않았다"만 말해서 `[sandbox]`에 아무 키나 있어도 참이었고, 새것은 예제가 적어 둔 provider 이름이 실제로 무엇으로 해석되는지를 말한다. 같은 테스트의 `named_agents == ["reviewer"]`는 그대로다. |
| A5 | `an_enabled_id_this_build_does_not_provide_fails_at_startup` (`rivet-cli/tests/plugin_cmd.rs:138`, 픽스처 `:143`) | **픽스처.** "이 빌드가 제공하지 않는 id"의 예로 `rivet.tool-shell`을 쓰는데 4.6이 그것을 카탈로그에 넣는다. `acme.tool-nonesuch`로 바꾼다 — `acme.` 네임스페이스는 이 저장소가 절대 싣지 않고, 같은 파일의 scaffold 테스트가 쓰는 `acme.tool-lint`와도 겹치지 않는다(겹쳐도 tempdir가 달라 무해하지만, 두 테스트가 같은 문자열을 다른 뜻으로 쓰면 읽는 사람이 멈춘다). **약속**: Phase 2가 없앤 "enabled인데 로드 불가"라는 중간 범주가 없다 — id는 로드되거나 오타이고, 오타는 startup에서 exit 2다. **그 뒤에 붙드는 것**: 같은 테스트, 같은 단언 셋(`code == 2`, stderr가 그 id를 담고, stderr가 `rivet.tool-filesystem`을 담아 "무엇이 있는지"를 말한다). 픽스처만 바뀐다. |

##### B. 바뀌지 않는다 — 왜 안 바뀌는지가 근거여야 한다

| 기존 단언 | 왜 안 바뀌는가 |
|---|---|
| `doctor_reports_what_the_configuration_resolved_to` · `a_readonly_profile_does_not_offer_the_write_tool` · `doctor_prints_what_each_plugin_actually_registered` · `a_readonly_profile_leaves_write_file_unregistered` (`rivet-cli/tests/e2e.rs:138`·`:153`·`:166`·`:189`, exit 0 단언 `:142`·`:157`·`:172`·`:196`·`:200`) | **그것이 §3.8의 판정 규칙이 존재하는 이유다.** 넷 다 `doctor`의 exit 0을 단언하고, 하네스 설정은 `enabled = ["rivet.model-openai","rivet.tool-filesystem"]`이라 `sandbox-local`이 로드되지 않는다. "provider 미등록 = `healthy = false`"였다면 넷이 동시에 2가 된다. 조건 ①이 그것을 막는데, **조건 ①이 `instance_id.is_some()`을 포함할 때만** 막는다 — `catalog::load`는 `validate()`를 카탈로그 전체에 돌리므로 로드되지 않은 `sandbox-local`·`tool-shell`·`tool-git`의 `effective`에도 `developer`에서는 `ProcessSpawn`이 남아 있고, 필터가 없으면 넷이 정확히 다시 깨진다(§3.8). 이 넷이 우연히 지켜지지 않도록 §6.3의 `doctor_is_healthy_when_no_loaded_plugin_can_spawn`이 같은 성질을 이름으로 붙든다. `doctor.rs:35`의 출력 문구는 이번에 바뀌지만 `"  profile     "` 접두는 유지하므로 `contains("profile     developer")`는 그대로 맞는다. |
| `doctor_prints_what_each_plugin_actually_registered`의 `!stdout.contains("reported")` (`e2e.rs:182-185`) | 그 문자열은 `record.claim_matches_reality()`가 거짓일 때만 나온다(`doctor.rs:132-136`). 하네스는 새 plugin을 하나도 로드하지 않으므로 애초에 후보가 없다. 그래도 §4.1의 네 매니페스트와 각 plugin의 `PluginHandle`이 실제 등록과 어긋나면 이 문자열이 나타나는 경로는 살아 있고, 그것을 `with_no_config_file_...`(아래)이 밟는다. |
| `with_no_config_file_every_plugin_the_default_selection_names_is_loaded` (`e2e.rs:209`) | **지난 표에 없던 줄이다.** 설정 파일이 **없는** tempdir에서 `doctor`를 돌리므로, 기본 선택이 셋에서 일곱이 되는 이 설계에서 **네 새 plugin이 처음으로 실제로 로드되는 자리**다. 그런데 깨지지 않는다: 단언은 다섯 개의 `+ …` 등록이 있다는 것과 `subscriber:telemetry.log`가 없다는 것뿐이고, 종료 코드는 보지 않는다. 다섯 등록은 그대로 나오고 telemetry는 여전히 기본 밖이다. **바뀌지는 않지만 새로 덮는다**: `catalog::load`는 어느 plugin의 실패든 전체 실패로 바꾸므로, 네 새 plugin 중 하나라도 로드에 실패하면 `+ tool:read_file`이 사라져 이 테스트가 실패한다. 즉 이 줄은 이번 Phase에 **처음으로 네 plugin의 로드 경로를 지키는** 테스트가 된다. |
| `plugin_list_does_not_tell_you_to_enable_what_you_already_enabled` (`e2e.rs:548`) | 단언 둘 다 통과한다. 앞 절반(`enable_plugins`로 telemetry를 켠 뒤 footer가 telemetry를 말하지 않아야 함)은 오늘 **공허하게** 참이다 — 오늘은 꺼진 plugin이 없어 footer 줄 자체가 인쇄되지 않고 `find(...)`가 `""`를 준다(`plugin_cmd.rs:70-81`, footer 는 `:77-81`의 한 줄). Phase 4 뒤에는 `off = [tool-shell, tool-git, policy-default, sandbox-local]`이라 footer가 **실제로 인쇄되고**, 그 줄에 telemetry가 없다는 것이 진짜 단언이 된다. 뒤 절반(설정이 telemetry를 안 켰을 때 footer가 그것을 말해야 함)은 footer가 한 줄에 쉼표로 이어 붙이므로 그대로 참이다. |
| `the_telemetry_plugin_is_not_in_the_default_selection` (`e2e.rs:252`) | `rivet.toml`을 지우고 `plugin list`를 돌려 telemetry 행의 `ENABLED` 열이 `no`임을 본다. 기본 선택이 커져도 telemetry는 밖에 남는다(§3.8). 행 포맷도 안 바뀐다. |
| `every_catalog_manifest_parses_and_declares_a_unique_id` · `every_default_selection_id_is_in_the_catalog` (`catalog.rs:227`·`:272`) | 둘 다 **자기 참조**다 — `ids.len() == sources().len()`, 그리고 기본 선택의 각 id가 카탈로그에 있는지. 리터럴이 없으므로 깨지지 않는다. **새로 덮는다**: 네 새 매니페스트가 파싱되지 않거나 id가 겹치면 첫째가, 기본 선택에 오타가 있으면 둘째가 잡는다. |
| `an_absent_enabled_list_selects_the_default_not_the_whole_catalog` (`catalog.rs:258`) | `chosen == default_selection()`(자기 참조)과 `chosen.len() < sources().len()`. 3 < 4가 7 < 8이 된다. 여유가 `telemetry-log` 하나로 줄어드는 것은 사실이고 §3.8이 그것을 적는다. 줄어들 뿐 깨지지 않는다. |
| `every_id_the_shipped_example_enables_resolves` (`catalog.rs:322`) | 예제에 더하는 네 id가 전부 카탈로그에 들어가므로 `select`가 성공한다. 이 테스트가 예제를 정직하게 유지하는 장치이고, A3이 더하는 관계 단언과 짝이다. |
| `the_context_plugin_is_in_the_catalog_and_always_selected` · `an_opt_in_plugin_is_absent_by_default_and_loads_when_named` · `an_id_this_build_does_not_provide_names_the_ones_it_does` (`catalog.rs`) | 각각 context plugin, telemetry, 오타 id에 대한 단언이고 카탈로그 크기에 의존하지 않는다. |
| `plugin_list_works_without_a_credential` · `plugin_list_says_which_ids_the_config_actually_enables` · `plugin_show_*` · `plugin_new_*` (`tests/plugin_cmd.rs`) | 전부 행 단위 `contains`이거나 scaffold가 만든 파일에 대한 단언이고, 카탈로그 크기에 단언이 없다. |
| `a_prompt_runs_a_tool_and_the_model_sees_its_result` (`e2e.rs:14`, 단언 `:42-56`) | **가장 위험해 보이는 줄이고, 그래서 근거를 적는다.** 세션 이벤트 이름 열 개를 **정확 비교**하므로 `approval.*` 둘이 끼면 깨진다. 끼지 않는다: 하네스 설정이 `rivet.policy-default`를 켜지 않으므로 `Registry::policies()`가 비어 있고, 체인은 baseline → `Allow`로 끝나 6단계에 들어가지 않는다. `tool.policy.evaluated`는 **버스** 토픽이지 세션 이벤트가 아니므로(`ToolEvent`, `rivet-core/src/event.rs:230`) 이 목록에 나타날 수 없다. 나머지 `session_events()` 단언은 전부 `contains`다. |
| `a_denied_path_is_refused_and_the_model_is_told_why` (`e2e.rs:61`) | `.env`를 읽으려는 호출이 `Blocked by policy`를 모델에게 보이고 세션에 `tool.blocked`를 남긴다고 단언한다. 하네스에 정책이 0개이므로 그 거부는 지금처럼 `fsguard`가 8단계에서 올리는 것이고 `WORKSPACE_POLICY`가 이름이다(§3.2가 그 상수를 남기는 이유). 문구는 `Disposition::content()`의 `"Blocked by policy: {reason}"`이라 체인이 만든 거부에도 같은 접두가 붙으므로, 설령 나중에 이 워크스페이스가 `policy-default`를 켜도 이 단언은 계속 맞는다. |
| SIGKILL 복구 · SIGINT 취소 e2e (`a_killed_run_is_closed_with_a_synthetic_result_and_resumes` `e2e.rs:662` · `ctrl_c_stops_an_in_flight_tool_and_exits_130` `e2e.rs:751`) | 세션 이벤트를 `contains`/`filter().count()`로만 본다. 하네스에 정책이 없으므로 이벤트 순서가 바뀌지 않는다. |
| `jsonl_output_is_one_parseable_event_per_line` · `jsonl_carries_a_failed_plugin_load_and_not_just_the_exit_code` · `session_list_and_show_read_the_durable_log` (`e2e.rs`) | 줄마다 파싱되는지, 특정 토픽이 `any()`로 있는지, 줄 수가 `>= 5`인지만 본다. 스트림에 `tool.policy.evaluated`가 더 실려도 통과한다. |
| `the_telemetry_plugin_logs_a_run_when_enabled` (`e2e.rs:290`) | 레코드 집합에 대한 `any()`/`!any()`이고 정확 비교가 아니다. `telemetry-log`의 `tool_detail`은 세 Phase 4 토픽에 대한 arm을 **이미** 갖고 있다(`record.rs:171-183`) — 와일드카드가 아니라 명시 arm이므로 새 토픽이 와도 조용히 떨어지지 않고, 이번 Phase에 그 코드가 처음 돈다. |
| `a_run_publishes_every_agent_and_tool_topic_this_phase_owns` (`event_flow.rs:170`) | 토픽별 `any()`이지 정확 목록이 아니므로 새로 실리는 `tool.policy.evaluated`가 끼어도 통과한다. 정확 목록을 쓰는 `a_host_lifecycle_publishes_...`(`:231`)는 도구 호출이 없어 영향이 없다. 이 테스트의 목록은 **늘리지 않는다** — Phase 4의 세 토픽에 대한 양성 커버리지는 §6.3의 `every_call_publishes_a_policy_decision`·`the_bus_carries_both_approval_topics`가 지고, 승인 토픽을 여기에 끼우려면 이 스크립트에 정책을 하나 걸어야 하는데 그러면 이 테스트가 두 가지를 증명하게 된다. |
| `every_panel_is_filled_from_events_alone` · `the_three_panels_fit_an_eighty_column_terminal` · `the_status_bar_shows_only_what_events_carry` 외 `rivet-tui/tests/panels.rs` 전부 | 손으로 만든 envelope만 보고 `AppState::default()`에서 시작한다. §3.7이 더하는 `pending: Option<ApprovalView>`는 `None`으로 시작하고 `draw`가 `Some`일 때만 모달을 그리므로 이 테스트들의 화면은 그대로다. 세 Phase 4 토픽의 `apply` 처리는 `state.rs:242-264`에 **이미 있고**, 이 Phase가 하는 일은 그 커버리지를 §6.3에서 **더하는** 것이다. |
| `the_tui_crate_does_not_depend_on_the_runtime` (`rivet-tui/tests/independence.rs:17`) | `rivet-tui/Cargo.toml`을 **파싱해서** `rivet-*` 의존이 `rivet-core` 하나뿐임을 단언한다. §3.7의 `ApprovalSink`는 `rivet-core`의 계약이므로 새 의존이 생기지 않는다. |
| `HumanRenderer` 경유 e2e stdout/stderr 단언 | 렌더러의 `match`에 `_ => {}`가 있어(`render/human.rs:86`) 세 Phase 4 토픽은 출력에 나타나지 않는다. |
| `rivet-runtime`의 `RunConfig` 사용처 (`tests/agent_loop.rs:41` · `event_flow.rs:43` · `session_recovery.rs:356`·`:417`·`:447`·`:481`) | 여섯 곳 전부 `RunConfig::new(..)`를 지나므로 새 필드는 그 생성자의 기본값으로 채워진다. `DispatchCtx` 리터럴은 `agent_loop.rs:576` 한 곳뿐이고 그것은 이 설계가 직접 고치는 자리다. |
| `registry.tool(..)`의 나머지 호출 지점 (`rivet-plugin/tests/loader.rs:236`·`:243`·`:260`) | 반환형이 `Option<Arc<dyn Tool>>` → `Option<RegisteredTool>`로 바뀌지만 이 셋은 `.is_some()`/`.is_none()`만 부르므로 깨지지 않는다. `.spec()`을 부르는 곳은 넷뿐이고(§3.2) 그 넷이 고쳐진다. |
| `.spec()`의 **다른** 호출 지점 (`rivet-plugin/src/guard.rs:218` · `tool-filesystem/src/lib.rs:80`·`:100`·`:186-196`) | 이 넷은 `registry.tool()`이 아니라 자기가 들고 있는 `Arc<dyn Tool>`에 대고 부른다. 반환형 변경과 무관하다 — §3.2가 세는 "호출 지점 넷"과 헷갈리기 쉬워서 여기 적는다. |
| `registry.rs` 단위 테스트 전부 (`registered_tools_are_resolvable_by_name` 등) | `is_some()`/`is_none()`/`tool_names()`만 쓴다. `tool_names()`는 `Table::names`를 지나므로 테이블이 무엇을 담든 같다. |
| `register_policy`를 부르는 유일한 기존 자리 (`rivet-plugin/tests/support/mod.rs:468`) | §3.2가 `register_policy`에 `"host.baseline"` 이름 거부를 더하는데, 이 자리는 `NamedPolicy("sneaky")`를 등록한다. 워크스페이스 전체에서 `register_policy`를 부르는 곳은 여기 하나뿐이므로(프로덕션 호출자는 아직 로더밖에 없다) 새 거부가 걸리는 기존 등록은 없다. |
| `every_topic_is_granted_or_deliberately_withheld` (`config.rs:614`) | 세 Phase 4 토픽에 대한 arm을 **이미** 갖고 있고(`:636-637`) `true`로 답한다. 프로파일의 `subscribable_topics`가 `"tool."` 접두를 주므로 실제로도 참이다. `ProcessSpawn` 추가는 `EventsSubscribe` 축과 무관하다. |
| `only_the_writable_profiles_may_subscribe_to_the_model_output` (`config.rs:691`) | `EventsSubscribe`만 본다. |
| `a_readonly_profile_strips_the_filesystem_plugins_write_permission` (`catalog.rs:338`) | `effective`를 `allows(FsWrite)`/`allows(FsRead)`로만 보고 집합을 통째로 비교하지 않으므로, `readonly`에 `ProcessSpawn`이 생겨도 그대로다. |
| `every_permission_has_a_rendering` (`plugin_cmd.rs:458`) | `Permission::ProcessSpawn => "process_spawn"` arm이 이미 있다(`plugin_cmd.rs:241`). 어휘를 넓히지 않으므로 그대로다. |
| `no_enabled_list_means_every_plugin_the_build_provides` · `an_enabled_list_is_deduplicated_in_first_seen_order` · `a_plugin_table_may_not_set_the_reserved_agent_key` · `a_plugins_config_carries_the_agent_object` · `only_ci_is_unattended_by_itself` · `headless_makes_any_profile_unattended` (`config.rs`) | 전부 손으로 쓴 설정 문자열이고 `rivet.example.toml`을 읽지 않는다. `[sandbox]`가 `toml::Table`에서 타입 있는 섹션이 되는 것도 이들에겐 보이지 않는다 — `FileConfig`에 `deny_unknown_fields`가 없으므로 파싱 표면이 좁아지지 않는다. |
| `every_ending_has_a_distinct_code` (`exit.rs:41`) | `for_stop(&StopReason::PolicyBlocked{..}) == 4`를 단언한다. §7-5는 Phase 4가 그 `StopReason`을 **만들지 않기로** 정했을 뿐 `for_stop`의 arm을 없애지 않으므로 그대로다. 바뀌는 것은 그 arm 위의 주석뿐이다. |
| `the_default_log_filter_only_raises_the_telemetry_plugin` (`main.rs:393`) | `DEFAULT_LOG_FILTER.matches('=').count() == 1`을 단언한다. 이 설계는 `tracing::warn!`(체인)과 `tracing::error!`(teardown 없는 drop)만 쓰고 둘 다 기본 `warn` 아래에서 보이므로 필터를 건드릴 이유가 없다. 건드리면 이 줄이 먼저 실패한다 — 그것이 이 줄의 용도다. |
| `rivet-core`·`rivet-session`·`rivet-plugin`의 단위/통합 테스트 전부 | §3.1이 `rivet-core`에서 고치는 것은 doc 세 줄이고 타입·wire 표현·트레이트 시그니처가 그대로다. `rivet-session`은 이 Phase가 건드리지 않는다. `rivet-plugin`은 로더 계약이 그대로이고, `manifest.rs:323`·`plugin.rs:242`가 픽스처로 쓰는 `rivet.tool-git`이라는 **문자열**은 카탈로그를 지나지 않으므로 4.7이 그 id를 실제로 실어도 충돌하지 않는다. |
| 워크스페이스의 doc 테스트 14개 | 전부 `running 0 tests`다 — 이 저장소의 코드 펜스는 `text`이지 `rust`가 아니다. 새 doc에도 같은 관례를 쓴다. |

### 6.1 DoD 6줄 — 각각 무엇이 증명하는가

| DoD (plan.md 원문) | 증명하는 테스트 |
|---|---|
| `--headless`에서 승인 요구가 매달리지 않고 거부됨 | `a_headless_run_denies_an_approval_instead_of_waiting` (`rivet-runtime/tests/approval.rs`) — `unattended: true`, sink는 부르면 `panic!`하는 스파이. `Disposition::Blocked`이고 sink가 한 번도 안 불렸음을 단언한다. 그리고 e2e: `headless_refuses_a_destructive_command_without_hanging` (`rivet-cli/tests/e2e.rs`) — 실제 서브프로세스를 `--headless`로 띄워 모델이 `shell{command:"rm -rf /"}`를 요청하게 하고, **벽시계 상한 안에** 종료하며 세션에 `tool.blocked`가 있음을 단언한다. 이것이 계획 문서의 수동 검증 `rivet --headless "rm -rf /"`를 대체하는 테스트다 (§6.2). |
| 승인/거부가 세션 로그에 durable event로 남음 | `an_approval_leaves_a_requested_and_a_resolved_in_the_log` (`tests/approval.rs`) — 승인·거부 두 경우에 대해 세션 이벤트 이름 순서를 통째로 단언하고, `actor`가 사람일 때 `Some`, 규칙일 때 `None`임을 본다. 짝: `the_bus_carries_both_approval_topics`, 버스 쪽 두 토픽. |
| resume 후에도 "세션 동안 기억" 승인이 유지됨 | `a_replayed_grant_skips_the_sink_on_the_next_run` (`rivet-runtime/tests/approval.rs`) — run 1에서 `ApprovedForSession`으로 답하고, 로그를 **다시 replay**해 `SessionState`를 만들고, run 2를 그 상태로 시작해 같은 `scope_key`의 호출이 sink를 **한 번도 부르지 않고** 실행됨을 단언한다. sink는 run 2에서 `panic!`하는 스파이다. 짝: `a_remembered_approval_does_not_cover_a_different_scope_key`. **이름이 `a_remembered_approval_survives_a_resume`가 아닌 이유**: 그 이름은 `crates/rivet-core/src/session.rs:763`에 **이미 있다.** crate가 달라 컴파일은 되지만 `cargo test a_remembered_approval_survives_a_resume`가 둘을 잡고, `docs/plan.md`가 DoD를 테스트 이름으로 인용하는 관례(`plan.md:186`·`:235`, 그리고 `:144`·`:147`이 그 예다)에서 어느 쪽을 가리키는지 모호해진다. 두 테스트가 증명하는 것도 다르다: core의 것은 projection이 로그에서 **만들어진다**를 보고, 이쪽은 그 projection이 다음 run에서 **소비된다**를 본다. 이름이 그 차이를 말하게 둔다. |
| 취소 시 프로세스 트리 전체 종료 (좀비 없음) | `cancelling_kills_the_whole_process_tree` (`plugins/sandbox-local/tests/kill.rs`, `#[cfg(unix)]`) — `sh -c 'sleep 300 & echo $! > pid; sleep 300'`을 띄워 손자 pid를 파일로 받고, 취소한 뒤 **손자가 사라졌음**을 `kill(pid, 0)` 대신 `/proc` 또는 `ps` 없이 확인 가능한 방식(자식이 쥐고 있던 파이프의 EOF)으로 단언한다. 그리고 런타임 쪽: `an_abandoned_tool_task_still_gets_its_sandbox_torn_down` (`rivet-runtime/tests/sandbox.rs`) — 취소를 무시하는 도구를 유예 시간 뒤 버리고, `teardown` 스파이가 그래도 불렸음을 본다. **이 두 번째가 §2.1-e의 실질이다.** |
| 거부된 도구가 세션에 `ToolBlocked`로 남고 모델이 사유를 봄 | `a_denied_call_is_blocked_and_the_model_reads_the_reason` (`rivet-runtime/tests/policy.rs`) — 세션에 `tool.blocked{policy: "default.destructive"}`가 있고, 다음 요청의 메시지 배열에 `Blocked by policy: …`를 담은 tool result가 있음을 단언한다. 후자가 DoD의 "모델이 사유를 봄"이다. |
| `readonly` 프로파일에서 `write_file`이 실제로 거부됨 | 두 층이므로 두 테스트다. 등록층은 Phase 2의 `a_readonly_profile_leaves_write_file_unregistered`가 이미 지킨다. **정책층**은 `default.grant`가 만든다(§2.1-g, §3.3) — 그것이 없으면 강제로 등록된 `write_file`은 `readonly`에서 그냥 돈다(`tool-filesystem`은 실행 시점에 `ctx.permissions()`를 읽지 않는다). `a_readonly_profile_denies_a_write_even_if_the_tool_is_registered` — **`plugins/policy-default/tests/grant.rs`에 산다, `rivet-runtime/tests/`가 아니다.** `readonly` grant로 `write_file` 모양의 도구를 **직접 레지스트리에 넣고**(plugin 로더를 지나지 않으므로 등록층이 개입하지 않는다) 진짜 `policy_chain::evaluate`를 돌려 `default.grant`가 거부함을 단언한다. 등록에만 의존하면 "다른 plugin이 같은 이름의 도구를 등록하면?"에 답이 없다. **자리가 저쪽인 이유는 의존 방향이다**: `default.grant`는 `policy-default`에 살고, `rivet-runtime`이 그것을 부르려면 런타임이 plugin을 dev-dependency로 잡아야 한다 — 이 저장소의 화살은 `plugin → rivet-runtime`(`tool-filesystem`이 `fsguard` 때문에 이미 그 edge를 갖는다)이고 그 반대는 테스트에서도 만들지 않는다. 반대로 `policy-default`가 `rivet-runtime`을 **dev-dependency**로 갖는 것은 같은 방향이고, `Registry::scoped(Owner{..})`가 공개 API이므로 그 테스트에서 레지스트리를 손으로 채울 수 있다. §3.9의 plugin `Cargo.toml` 행에 그 한 줄이 있다. |

### 6.2 진행 추적 표의 게이트 — 심볼릭 링크 탈출 차단

Phase 1.6b가 닫은 것은 **파일을 여는 경로**다. Phase 4가 여는 새 표면은 **프로세스의 cwd와
git의 경로 인자**이고, 게이트는 거기 붙는다.

- `a_symlinked_cwd_pointing_outside_the_workspace_is_refused`
  (`plugins/sandbox-local/tests/cwd.rs`, `#[cfg(unix)]`) — 워크스페이스 안에 밖을 가리키는
  디렉터리 링크를 만들고 그것을 `ExecSpec.cwd`로 준다. `PolicyDenied`.
- `a_symlinked_cwd_pointing_inside_the_workspace_is_allowed` — 안을 가리키는 링크는 통과한다.
  거부하는 것은 안전하지만 근거가 없다(`fsguard`의 모듈 doc이 같은 문장을 적는다).
- `a_git_path_argument_cannot_leave_the_workspace_through_a_link`
  (`plugins/tool-git/tests/paths.rs`, `#[cfg(unix)]`).
- `the_default_policy_refuses_an_escaping_cwd_before_the_sandbox_sees_it`
  (`plugins/policy-default/tests/workspace.rs`) — 어휘 층이 먼저 거른다는 것. 두 층이
  독립적으로 작동함을 보이는 것이 목적이고, 그래서 이 테스트에는 파일시스템이 없다.

windows는 `#[cfg(unix)]`로 빠진다 — 심볼릭 링크 생성에 권한이 필요하고, `fsguard`가 같은
이유로 같은 결정을 이미 했다. 문서에 "windows 미검증"을 유지한다.

### 6.3 작업 항목별

**세 Phase 4 토픽이 실제로 발행된다.** §6.0의 A1이 고치는 tripwire는 "발행자가 있다고
주장한다"까지만 보므로, 주장이 참인지는 따로 본다:
`every_call_publishes_a_policy_decision` (`rivet-runtime/tests/policy.rs` — 정책이 0개인
레지스트리에서도 `tool.policy.evaluated`가 호출마다 **정확히 한 번** 실리고 `policy` 필드가
`"host.baseline"`이다), `the_bus_carries_both_approval_topics` (`tests/approval.rs`, §6.1의
DoD 2 항목과 같은 테스트). 이 둘이 없으면 tripwire를 `Published`로 옮기는 편집이 거짓말이
된다. 짝으로 `no_registered_policy_may_claim_the_baseline_name` — `host.baseline`이라는
이름의 정책을 등록하면 `register_policy`가 거부함을 단언한다. 그 이름이 "아무도 결정하지
않았다"를 뜻하는 한, 그것을 가로챌 수 있으면 로그가 거짓말한다.

**4.1 Policy chain fold.** `combine`의 성질은 `rivet-core`가 이미 테스트한다. 런타임이
증명할 것은 **fold를 제대로 돌리는가**다: `the_chain_evaluates_every_policy_not_just_until_a_deny`
(거부가 나와도 나머지가 평가되어 constraint가 합쳐짐), `the_baseline_is_the_seed_not_an_override`
(정책이 baseline보다 짧은 timeout을 주면 그것이 이기고, 긴 timeout을 줘도 baseline이 이김),
`the_deciding_policy_is_named_in_the_block` (`ToolBlocked.policy`가 실제로 거부한 정책 이름),
`a_rewrite_is_re_evaluated_before_it_runs` (rewrite가 넓히려 하면 두 번째 라운드가 잡음),
`a_rewrite_loop_denies_at_the_depth_limit`,
`a_cancelled_chain_closes_the_call_as_not_started` (체인 도중 취소 — 원본을 돌리지 **않는다**,
§5-9b), `an_unattended_require_approval_becomes_a_deny`.

**4.2 Interceptor 실행.** `a_hanging_interceptor_is_treated_as_an_abstention`
(2 s보다 오래 자는 interceptor; 호출은 진행되고 경고가 남음),
`interceptors_run_concurrently_not_serially` (각각 1.5 s 자는 interceptor 셋을 걸고 체인이
2.5 s 안에 끝남을 단언 — 직렬이면 4.5 s. 여유는 넉넉히 두고 상한은 체인에만 건다),
`an_interceptor_cannot_widen_a_policys_deny` (`RestrictiveDecision`에 `Allow`가 없다는 것의
런타임 판),
`interceptor_order_changes_which_reason_is_first_not_the_outcome`.

**4.3 기본 정책.** `policy-default`의 단위 테스트. `default.workspace`: 경로 키 집합, 탈출
경로, deny 목록 적중, 배열 인자. `default.grant`:
`a_mutating_tool_is_denied_without_fs_write`(다섯 프로파일의 grant × `read_only` 두 값의
표를 통째로 단언), `a_read_only_tool_passes_under_every_profile`,
`a_subtree_write_grant_still_counts_as_write`, 그리고 한계를 적어 두는
`a_tool_that_lies_about_being_read_only_passes_this_layer` — 통과함을 **단언하는** 테스트이고,
이름과 주석이 왜 그것이 여기서 막을 일이 아닌지(신뢰 경계, §2.1-g) 적는다. 그 짝으로
반대 갈래도 못 박는다: `a_tool_that_declares_no_annotations_is_treated_as_mutating` —
`ToolSpec::new`만 부르고 `with_annotations`를 안 부른 도구는 `read_only: false`이므로
(`ToolAnnotations::default()`, `tool.rs:63`·`:80-85`) `readonly`에서 막힌다. 이건 버그가
아니라 fail-closed이지만 **말없이 그렇게 되면 안 되므로**, 이 테스트는 거부된다는 것과
거부 사유가 "이 도구는 `read_only`를 선언하지 않았다"를 말한다는 것을 함께 단언한다
(§3.3의 두 번째 갈래, `plugin.md`의 새 문장).
`default.destructive`: 파괴적 형태 목록의 각 항목, `write_file`이 통과하고 `git_commit`이
걸리는 것, `production`에서 읽기 도구까지 승인 대상이 되는 것,
`a_shell_gate_is_never_rememberable`(§3.3 표의 2번 줄), 그리고
`the_matcher_is_not_a_boundary` — `rm -r -f` 변형은 잡고 `rm${IFS}-rf`는 **잡지 못함을
단언하는** 테스트. 실패를 문서화하는 테스트이고, 이름과 주석이 그 이유를 적는다.

세 정책이 함께 도는 것도 본다: `the_three_default_policies_fold_to_the_strictest`
(`grant`가 `Deny`, `destructive`가 `RequireApproval`일 때 결과가 `Deny`이고 `deciding`이
`default.grant`임을 단언).

**4.4 승인 UI.** `rivet-tui`: `an_approval_modal_is_drawn_from_state_alone`(손으로 만든
`AppState`로 `TestBackend`에 그린다), `the_keys_map_to_the_three_outcomes`,
`a_dropped_receiver_clears_the_pending_modal`. `rivet-cli`:
`the_prompt_goes_to_stderr_not_stdout`, `a_non_tty_stdin_produces_no_sink_and_marks_unattended`.
그리고 6단계의 순서: `a_remembered_approval_is_honored_even_when_unattended`
(§2.1-c의 ①이 ②보다 앞이라는 것 — 뒤집는 판단은 §7-13에 있으므로, 이 테스트가 그
결정을 붙드는 자리다), 그리고 임베더용 갈래
`an_approval_with_no_sink_is_denied_even_when_attended` (`tests/approval.rs`) —
`unattended: false`인데 sink가 없는 조합은 CLI가 만들지 않지만 `rivet-runtime`을 직접 쓰는
호스트는 만들 수 있고, 조용히 통과하면 안 된다 (§3.2).

**4.5의 런타임 쪽 — 샌드박스 경계.** `a_call_that_never_spawns_is_not_blocked_by_a_missing_sandbox`
(`rivet-runtime/tests/sandbox.rs`) — provider를 하나도 등록하지 않은 레지스트리에서
`read_file`류 도구가 정상 완료하고 `tool.execute.started.sandboxed == false`임을 단언한다.
**이것이 `production`과 구형 `rivet.toml`을 지키는 테스트다** (§2.1-a′). 짝:
`a_call_that_spawns_without_a_sandbox_fails_naming_the_provider`,
`a_policy_named_provider_beats_the_configured_default`,
`the_scope_is_sealed_after_teardown`(teardown 뒤 `exec`가 `Err`, §5-10b).

**4.5 sandbox-local.** `guarantees_are_all_false`, `the_environment_starts_empty`,
`only_the_passthrough_names_survive`, `output_over_the_cap_is_truncated_and_the_child_is_not_blocked`
(상한의 10배를 쏟는 자식이 **완주함**을 단언 — 읽기를 멈추면 여기서 걸린다),
`a_timeout_kills_the_group_and_reports_timed_out`,
`teardown_is_idempotent`, `prepare_refuses_without_process_spawn`, 그리고 §6.2의 두 개.

**4.6 shell.** `the_tool_never_spawns_directly`(`ctx.host.exec` 스파이가 불렸음),
`sh_minus_c_is_explicit_in_the_argv`(주입 표면이 로그에 드러남),
`a_narrower_timeout_wins_over_the_context_budget`,
`a_non_utf8_stdout_is_reported_as_lossy`,
`the_shell_is_not_registered_without_write_permission` (readonly·reviewer·production 셋 다).

**4.7 git.** 네 도구 각각의 성공/비저장소 경로, `git_commit`이 `FsWrite` 없이는 등록되지
않는 것, `the_manifest_declares_every_slot_this_plugin_registers`,
`every_spec_passes_the_runtimes_schema_check` (`tool-filesystem`이 가진 것과 같은 것).

**4.9 프로파일.** `every_profile_says_whether_it_may_spawn_a_process` — `Profile::all()` 위의
**와일드카드 없는 `match`**로 각 프로파일을 "준다/안 준다"로 분류하고 두 기대 목록과 대조한다.
Phase 3의 `is_narrowed`와 같은 모양이고, 같은 이유로 존재한다: 프로파일이 새로 생기면
컴파일에 실패하고, arm을 더하면 목록에 없어서 실행에 실패한다.
그리고 `a_reviewer_can_read_the_diff_but_not_commit`,
`the_profile_table_in_security_md_matches_the_code` — 다섯 프로파일 × (fs write · process ·
등록 도구 이름 목록)을 §4.5 표 그대로 단언한다.

**`doctor`의 판정 규칙 (§3.8).** 세 갈래를 전부 e2e로 못 박는다:
`doctor_is_healthy_under_production_even_though_no_sandbox_is_registered`(가장 닫힌
프로파일이 설정 오류로 보고되지 않는다),
`doctor_is_healthy_when_no_loaded_plugin_can_spawn`(하네스의 기본 설정 —
`enabled = ["rivet.model-openai","rivet.tool-filesystem"]` — 이 exit 0을 유지한다는 것을
**이름으로** 붙들어 둔다. 기존 넷이 같은 것을 우연히 지키고 있지만, 우연히 지켜지는 성질은
다음에 누가 규칙을 조일 때 다시 깨진다), 그리고
`doctor_fails_when_a_process_capable_plugin_has_no_sandbox_provider`
(`tool-shell`을 켜고 `sandbox-local`을 빼면 exit 2이고, 그 줄이 provider 이름을 말한다).

**tripwire.** `every_bus_topic_is_claimed`의 `owner`에서 세 `ToolEvent` 변형이
`Owner::Deferred(4)` → `Owner::Published`로 옮겨지고, 두 기대 목록이 그에 맞게 바뀐다.
그 편집이 없으면 테스트가 실패한다 — 그것이 과제 서술이 말한 "Phase 3이 남긴 tripwire가
강제한다"의 실체다. 남는 `Deferred`는 `job.*` 다섯뿐이다.

**계획 문서의 수동 검증 표면 (D).** 둘 다 테스트로 대체한다.
`rivet --profile readonly "delete all logs"` → `readonly_offers_no_write_tool_and_the_model_is_told`
(e2e, 루프백 provider가 `write_file`을 요청하면 "unknown tool" 결과를 받고 도구 목록에
`write_file`이 없음을 단언). `rivet --headless "rm -rf /"` → §6.1의 e2e.
**실제 provider 호출 없이** 이 두 경로가 성립함을 보이는 테스트이고, 과제 서술이 허용한
대체다. 그렇게 대체했다고 여기 적는다.

### 6.4 게이트

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # 새 공개 타입은 전부 Debug 필요
cargo test --workspace                                  # 0 failed, 579 이상 passed
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

`579`는 base `4c7bfae`에서 실측한 수다(§6 머리말). 과제 서술이 인용하는 559는 Phase 3
종료 시점의 수이고 이 브랜치의 base가 아니므로, 그것을 목표로 삼으면 스무 개가 사라져도
게이트가 통과한다.

`missing_debug_implementations`가 `warn` + `-D warnings`이므로 `Baseline` · `Evaluated` ·
`Approvals` · `SandboxScope` · `RegisteredTool` · `LocalSandbox` · `LocalHandle` ·
`ApprovalView`는 전부 `Debug`를 가진다. `SandboxScope`는 `Mutex<ScopeState>`와
`Arc<dyn Sandbox>`를 들므로 수동 구현이다.

타이밍에 의존하는 테스트는 셋이다 — interceptor 타임아웃, 동시성, 취소. 각각 여유를 최소
2배 두고, 그 선택과 "상한은 체인/런타임에만 걸고 자식 프로세스에는 안 건다"는 결정을 테스트 안
주석에 적는다. `cargo test --workspace`가 CI(ubuntu)와 개발 머신(darwin) 양쪽에서 돌아야
하므로 프로세스 관련 테스트는 `sh`·`sleep`만 쓰고 GNU 확장을 쓰지 않는다.

---

## 7. 열린 질문

과제 서술이 정하지 않은 것들. 각각 이 설계가 쓰는 잠정 답을 **[가정]**으로 달아 두어 빌드가
막히지 않게 하되, 가정임을 숨기지 않는다.

1. **어떤 인자가 경로인지 스키마가 말해주지 않는다.** `default.workspace`는 키 이름
   (`path` · `cwd`)으로 추측한다. 도구가 `target_file`이라고 이름 붙이면 정책이 못 본다.
   **[가정]** 선언된 키 집합으로 가고, 그것을 plugin의 doc과 `plugin.md`에 적는다. 진짜 답은
   `ToolSpec.input_schema`에 `x-rivet-path` 같은 주석을 허용하는 것인데, 그건
   `schema::validate_spec`의 닫힌 어휘를 넓히는 일이라 도구 저자가 실제로 요구할 때 연다.
2. **provider 이름 사이에는 순서가 없다.** `ExecutionConstraints::merge`의 sandbox 축은
   `self.or(other)`이므로 서로 다른 두 이름 중 왼쪽이 남는다 — rewrite는 다르면 `Deny`인데
   provider는 아니다. 이 설계는 그 성질 때문에 설정의 provider를 씨앗이 아니라 별도 필드로
   나르고(§2.1-a), 이 호출의 provider를 `constraints.sandbox ∨ 설정`으로 정한다. 그러면
   "정책이 `local`을 대고 설정이 `docker`인" 경우 정책이 **느슨한** 쪽으로 이긴다.
   **[가정]** 받아들인다 — provider가 하나뿐인 이 빌드에서는 관측될 수 없고, 고치려면
   `SandboxGuarantees`에 순서를 도입하거나 `merge`를 바꿔야 하는데 둘 다 `rivet-core`
   계약 변경이다. `docker`가 오는 Phase에서 정할 것 둘: 두 이름이 다르면 `Deny`인가,
   그리고 설정과 정책 중 어느 쪽이 바닥인가. §11 승격 후보.
3. **`SecretsRead`와 `JobManage`는 여전히 아무 프로파일도 주지 않는다.**
   [`§11-6`](../architecture.md)이 시크릿 저장·주입·마스킹을 "Phase 4"로 적었지만
   `docs/plan.md`의 4.1–4.9에는 시크릿 항목이 없다. **[가정]** 범위 밖. 작업 항목의 출처는
   plan.md이고, 게다가 Phase 4는 시크릿이 **필요 없게** 만든다 — 샌드박스가 빈 환경에서
   시작하고 운영자가 이름으로 명시한 것만 들어간다. 요청자가 없는 저장 경로를 설계하는 것은
   기능을 발명하는 일이다. §11-6은 열린 채로 두고, "Phase 4"라는 딱지만 갱신한다.
4. **interceptor 타임아웃/에러를 버스에도 알릴 것인가.** 새 토픽
   (`tool.interceptor.timed_out` 같은 것)은 닫힌 어휘를 넓히고 `every_bus_topic_is_claimed`와
   `every_topic_is_granted_or_deliberately_withheld` 양쪽을 건드린다. **[가정]**
   `tracing::warn!`만. 계약이 요구하는 것은 "보고된다"이고 `tracing`이 보고다. 토픽이 필요한
   근거가 생기면(운영자가 CI에서 세고 싶다든가) 그때 어휘를 넓힌다.
5. **`StopReason::PolicyBlocked`(exit 4)를 무엇이 만드는가.** Phase 4에서도 만들지 않는다 —
   거부는 도구 결과이고 모델이 적응한다(DoD 5). 그러면 exit 4는 여전히 도달 불가능하고,
   `for_stop`의 그 arm은 Phase 1의 주석("Phase 4 fills it in") 그대로 남는다.
   **[가정]** 만들지 않고, `exit.rs`의 그 주석을 "정책 거부는 run을 끝내지 않는다. 이 코드가
   필요해지는 것은 run 자체가 정책으로 거절될 때(예: `LoadPlugin`·`StartRun` 액션)이고 그건
   아직 없다"로 고친다. 스크립트가 "정책이 막았다"를 알고 싶다면 오늘의 답은 `--jsonl`의
   `tool.blocked`이고, 그걸로 충분한지는 이 설계가 정하지 않는다.
6. **`PolicyAction`의 나머지 세 변형을 누가 부르는가.** `LoadPlugin` · `StartRun` ·
   `AutoApproveJob`은 정의되어 있고 호출자가 없다. Phase 4가 채우는 것은 `ToolCall`뿐이다.
   **[가정]** 그대로 둔다. `LoadPlugin`은 Phase 6(외부 plugin)의 모양이고 `AutoApproveJob`은
   Phase 5다. `StartRun`은 오늘도 부를 수 있지만 부를 정책이 없다.
7. **`tool.execute.started.sandboxed`의 뜻.** 7단계가 레지스트리를 조회하므로 이 필드는
   "이 호출이 프로세스를 띄운다면 **등록된** `<provider>` 아래에서 돈다"이다 — `production`이나
   샌드박스 plugin이 없는 설정에서는 `false`이고, 그래서 공허하게 참인 경우는 "provider는 있고
   프로세스는 안 띄운" 호출뿐이다. **[가정]** 그 정의를 `events.md`에 적는다. 대안(실제 prepare
   여부를 싣기)은 lazy prepare 때문에 발행 시점에 알 수 없고, eager prepare는 `docker`에서 비싸다.
8. **승인 프롬프트에 타임아웃이 없다.** `ApprovalOutcome::TimedOut`은 취소·데드라인 경로에서만
   나온다. **[가정]** 별도 타임아웃 없음. run 데드라인이 이미 `ctx.cancel`을 통해 도달하므로
   발명할 숫자가 없고, 발명하면 "사람이 커피 타러 간 사이 거부됨"이라는 새 실패 모드가 생긴다.
9. **`--jsonl`에서 누가 승인에 답하는가.** clap이 `--jsonl`과 `--headless`를 배타로 두었으므로
   `--jsonl`은 무인이 아니다. **[가정]** stdin이 tty이면 stderr에 묻고, 아니면 §3.7 규칙으로
   무인이 된다. 즉 CI의 `--jsonl`은 자동으로 무인이 되고, 사람이 보는 `--jsonl`은 묻는다.
   `--jsonl` 스트림 자체로 승인을 주고받는 프로토콜(요청을 stdout에, 답을 stdin에)은 매력적이지만
   그건 스트림을 관찰용에서 제어용으로 바꾸는 일이고, `events.md`가 "관찰용이지 세션 재구성용이
   아니다"라고 적어 둔 성질을 건드린다.
10. **`readonly`에 `ProcessSpawn`을 주는 것이 맞는가.** [`security.md` §8](../security.md)의
    "제한적"을 이 설계는 "git 읽기 명령"으로 읽었다. **[가정]** 그렇게 읽는다 — `reviewer`가
    `git_diff`를 못 쓰면 `[agents.reviewer]` 예시가 성립하지 않고, `readonly`와 `reviewer`를
    이 축에서 다르게 두면 표의 두 칸("제한적" / "읽기 명령만")이 실제로는 같은 것을 뜻하게 된다.
    다르게 읽는다면(예: `readonly`는 프로세스 전면 금지) `Profile::permissions`의 한 줄이고,
    그 결정은 보안 결정이므로 리뷰가 정할 자리다.
11. **`rustix`를 새 의존으로 들이는 것.** `Cargo.lock`에 이미 1.1.4가 있으므로 빌드 비용은
    사실상 없지만, 직접 의존은 `cargo audit`의 표면을 늘린다. **[가정]** 들인다.
    대안은 ① `unsafe` 한 블록(워크스페이스가 `forbid`한다) ② `/bin/kill`을 띄우기(프로세스를
    죽이려고 프로세스를 띄운다) ③ `nix`(트리가 더 크다)이고, 셋 다 더 나쁘다.
    `[workspace.dependencies]`를 거치는 것은 가정이 아니라 이 저장소의 관례다 (§3.4).
12. ~~**`default_selection`에 네 개를 다 넣는 것.**~~ **결정됨 (리뷰 2).** 넷 다 넣는다.
    이 항목이 리뷰에 넘긴 것은 "설정 파일 없는 트리에서 모델이 셸을 갖게 된다"가 받아들일
    만한가였고, 답은 **경계가 달라지지 않는다**이다: 그 트리의 기본 프로파일은 `developer`이고
    그 프로파일은 이미 `FsWrite(Workspace)`를 준다. [`security.md` §3](../security.md)이
    `.git/config`에 대해 적은 문장("git 설정 쓰기 권한은 셸 권한과 같다")을 거꾸로 읽으면
    워크스페이스 쓰기 권한과 셸 권한은 같은 것이므로, 셸을 빼도 그 프로파일이 할 수 있는 일이
    줄지 않고 `docs/plan.md`의 MVP 도구 표만 거짓이 된다. 셸을 정말로 빼고 싶다면 빼야 하는
    것은 도구가 아니라 그 프로파일의 `FsWrite`다.
13. **기억된 승인 조회가 무인 변환보다 앞에 온다.** 그래서 `--headless`나 `ci`로 resume해도
    사람이 "세션 동안 허용"한 `scope_key`는 유효하다. **[가정]** 그 순서로 간다 —
    `unattended`는 "지금 아무도 답할 수 없다"이고 기억은 "이미 누가 답했다"이므로, 변환을
    먼저 하면 durable하게 남은 사람의 결정을 현재의 플래그가 지운다. 반대 순서도 방어
    가능하다: `Outcome::RequireApproval`의 doc이 "Only meaningful when `!unattended`"라고
    적었고, 무인 실행에서 아무것도 승인되지 않는 편이 닫히는 쪽이다. 뒤집는 것은
    `Approvals::resolve` 안의 두 줄을 바꾸는 일이고, 뒤집는다면 DoD 3의 테스트가 "attended
    resume에서만"으로 좁아진다는 것을 함께 적어야 한다.
14. **`ToolSpec`이 "이 도구가 요구하는 permission"을 선언하지 않는다.** 그래서
    `default.grant`는 `annotations.read_only`라는 대리 지표로 판단한다 (§2.1-g) — 쓰기가
    아니라 네트워크만 필요한 도구도 `read_only: false`면 `FsWrite` 없는 grant에서 막히고,
    **annotations를 아예 안 적은 도구도 같은 자리에서 막힌다**(그 기본값이 `false`이므로,
    §5-25b). 대리 지표의 대가는 이 둘이고 둘 다 fail-closed 쪽이다.
    **[가정]** 받아들인다. 이 빌드의 도구 중 그런 것은 없고, `readonly` 프로파일에서 "변이하는
    도구는 안 돈다"는 결론 자체는 옳다. 진짜 답은 `ToolSpec`에 `requires: Vec<Permission>`을
    더해 등록 시점에 grant와 대조하는 것인데, 그건 계약 변경이고 `PluginManifest.permissions`와
    역할이 겹치므로(하나는 plugin 단위, 하나는 도구 단위) 어느 층이 맞는지부터 정해야 한다.
    §11 승격 후보이고, [`§11-13`](../architecture.md)(도구별 경로 범위를 무엇이 강제하는가)과
    같은 자리를 가리킨다.
15. **[`§11-9`](../architecture.md)가 요구한 "의도적으로 다시 열기".** 그 항목은
    "Phase 4의 sandbox가 이걸 물려받기 전에 의도적으로 다시 열어야 한다"고 이름까지 적어 뒀다.
    다시 연 결과: **`sandbox-local`은 `network_isolation: false`이므로 도구 egress를 강제할
    수단이 이 Phase에도 없다.** 그래서 `security.md` §8 표의 `network` 열은 여전히 아무것도
    강제하지 않고, `production`의 "허용 목록"도 마찬가지다. **[가정]** 모든 프로파일이
    `NetworkHttp(None)`을 계속 준다. provider 호출만 따로 금지하려면 어휘에 별도 permission이
    필요하고 그건 `rivet-core` 변경이며, 강제할 수 있는 provider(`docker`)가 생기기 전에
    어휘를 넓히는 것은 §11-11에서 interceptor에 대해 내린 판단("요청자가 없는 어휘 확장은
    하지 않는다")과 같은 이유로 하지 않는다. Phase 4는 이 항목을 **닫지 않고**, 물려받았다는
    사실과 그 이유를 §11-9에 적는다.

---

## 8. 구현이 설계에서 벗어난 곳

위 본문은 승인된 그대로다. 아래는 구현하면서 갈라진 지점 전부이고, 각 항목은 **무엇에
부딪혔는가**와 **대신 무엇을 했는가**다. 설계의 방향을 바꾼 것은 하나도 없다 — 대부분은
설계가 말하지 않은 자리를 채운 것이고, 셋은 테스트를 쓰다가 드러난 실제 결함이다.

### 8.1 설계가 정하지 않아 채운 것

**정책이 `Err`를 돌려주면 거부다.** §5-6은 **interceptor**의 `Err`를 "기권과 같이 취급하고
보고한다"로 정했고, 정책의 `Err`는 어디에도 없다. `Outcome`에 기권 변형이 없으므로 `Allow`를
접는 것은 *돌지 않은 규칙으로부터 동의를 발명하는 일*이다. 정책 이름을 담은 `Deny`로 접고
`tracing::warn!`을 남긴다. interceptor와 다르게 두는 근거는 `RestrictiveDecision`에 `Allow`가
없다는 사실 자체다 — 기권이 interceptor에게는 일급 답이고 정책에게는 아니다.
테스트: `a_policy_that_cannot_decide_is_a_refusal_not_a_pass`.

**재작성의 고정점은 수렴이다.** §3.2는 "`REWRITE_DEPTH_LIMIT`을 넘기면 `Deny`"만 적었다.
그러면 같은 값을 계속 돌려주는 멱등 정책이 3라운드 뒤에 거부된다. 자기가 이미 가진 호출을
다시 요구하는 것은 변한 것이 없다는 뜻이므로 `rewritten == current`면 루프를 끝낸다. 깊이
제한은 **다른** 값을 계속 내놓는 경우에만 발동하고, `a_rewrite_loop_denies_at_the_depth_limit`이
라운드마다 값을 바꾸는 정책으로 그 갈래를 확인한다.

**`actor`에 무엇을 적는가.** §4.6의 로그 예시에 `actor: "kangwoo"`가 나오지만 그 이름의
출처는 어디에도 없다. 계약에 신원 서비스가 없으므로 `$USER` → `$USERNAME` → `"local"` 순으로
읽는다. `actor`가 `Some`인 것은 사람이 실제로 답한 경로뿐이므로(규칙이 답하면 언제나 `None`)
이 문자열이 틀린 자리에 나타날 일은 없다.

**`Approvals::resolve`의 인자 모양.** 설계는 `resolve(&self, ctx, session, call,
outcome_fields)`로 스케치했고, 구현은 `resolve(session, bus, Where, &Ask, unattended,
cancel)`이다. `DispatchCtx`를 통째로 받으면 그것이 `Approvals`를 담고 있어 읽는 사람이 순환을
의심하게 되고, 버스는 어차피 따로 필요하다. `Where`와 `Ask`는 그 두 묶음에 이름을 준 것이고,
①②③ 순서와 `apply_unattended`의 호출 위치는 그대로다.

**`dispatch`가 세 함수로 갈라졌다.** `clippy::too_many_lines`에 걸려 1·2·3단계를 `admit`으로,
4·5·6단계를 `judge`로 뽑았다. 순서도 각 단계가 하는 일도 그대로이고, 두 함수의 경계가 마침
설계가 "한 질문"이라고 부른 묶음과 같다 — `admit`은 "호출 가능한가", `judge`는 "일어나도
되는가, 어떤 형태로".

**`git_log`의 `limit`.** §4.3은 `shell`·`git_diff`·`git_commit`의 스키마만 적었다. `git_log`에
`limit`(1–200, 기본 20)을 더했다 — 상한 없는 `git log`는 출력 상한에 부딪혀 잘리고, 그러면
모델이 받는 것은 가장 오래된 커밋들이다.

### 8.2 설계의 표현이 실제로는 성립하지 않았던 것

**파괴적 명령 목록의 두 항목은 부분 문자열이 될 수 없었다.** §3.3의 기본 목록에 `curl … | sh`
와 `wget … | sh`가 있는데, 줄임표는 부분 문자열로 표현할 수 없고 이 목록은 **운영자가 통째로
교체하는 설정 값**이라 매칭 규칙이 항목마다 다르면 교체한 사람이 예측할 수 없게 된다. 목록을
전부 순수 부분 문자열로 두고 그 둘을 `| sh` · `| bash`로 적었다 — 위험한 것은 fetch가 아니라
셸로 들어가는 파이프이고, `curl` 자체를 잡으면 목록이 너무 자주 울려 무시된다. 같은 이유로
`sudo`와 `dd`에 꼬리 공백을 붙였다: 없으면 `pseudo_random`이 `sudo`에 걸려
`cargo test pseudo_random`이 승인 대상이 된다. 항목 수는 설계대로 15개다.

**`the_prompt_goes_to_stderr_not_stdout`을 그 이름으로 쓰지 않았다.** 프롬프트는 설계대로
stderr로 나가지만 그것을 관측하는 테스트를 쓸 수 없다 — 서브프로세스 e2e는 stdin이 파이프이고,
파이프면 §3.7의 규칙대로 **sink 자체가 만들어지지 않으므로** 프롬프트가 존재하지 않는다. 그
규칙이 stdout 오염에 대해서는 더 강한 보장이므로, `a_non_tty_stdin_produces_no_sink`와
`a_non_tty_stdin_marks_a_run_unattended`가 대신 붙든다. 프롬프트 문자열 자체는
`the_prompt_shows_what_will_happen_and_which_keys_answer`가 본다.

**TUI의 키 테스트가 `tests/`가 아니라 `app.rs`에 있다.** `handle_key`가 private이고, 테스트
하나 때문에 공개 API를 넓히는 것은 이 crate가 지켜 온 성질과 방향이 반대다. 그리기 테스트만
`tests/approval.rs`에 두고(손으로 만든 `AppState`가 전부다), 키와 왕복은 `app.rs`의 단위
테스트로 넣었다 — 그 파일에 이미 키 테스트가 넷 있다.

### 8.3 테스트를 쓰다가 발견한 실제 결함 셋

**상속된 파이프를 쥔 손자가 끝난 호출을 무한정 붙잡는다.** 설계에 없던 실패 모드다. 파이프는
**마지막** writer가 놓아야 닫히고 손자는 부모의 stdout을 물려받으므로,
`sh -c "sleep 300 & exit 0"`은 자식이 즉시 끝나도 손자가 파이프를 쥐고 있다. 출력 리더를
끝까지 `await`하면 이미 끝난 호출에서 디스패처가 영원히 멈춘다. 리더가 공유 버퍼에
append하도록 바꾸고, 자식을 reap한 뒤 `OUTPUT_SETTLE`(500 ms)만 기다린 뒤 버린다. 넘겼으면
`truncated`로 신고한다. 숫자를 발명한 것이 맞고, §5의 자기 기준으로 허용된다 — 초과의
대가("출력 꼬리가 없다")가 계약에 이미 `ExecOutput::truncated`로 쓰여 있다.

같은 자리에서 §6.1이 제안한 관측 방법도 바꿔야 했다. 읽기 전용으로 연 FIFO는 writer가 열
때까지 블록되므로 FIFO의 EOF로 손자를 관측하려는 테스트는 **통과하는 바로 그 경우에
교착**한다. 대신 스크립트가 손자 pid를 파일로 적고 테스트가 `kill -0`으로 확인한다 —
POSIX이고, `/proc`(darwin에 없다)도 `ps` 파싱(플랫폼마다 다르다)도 필요 없다.

**탈출하는 심볼릭 링크가 `git` 경로 인자에서 부모 검사로 샌다.** `tool-git`의 경로 정규화는
`fsguard::resolve_dir`를 먼저 시도하고 실패하면 파일 규칙으로 넘어간다. **모든** 실패에서
넘어가면, 워크스페이스 밖을 가리키는 디렉터리 링크에 대해 `resolve_dir`가 `PolicyDenied`를 낸
뒤 파일 규칙이 그 링크의 *부모*(= 워크스페이스 루트)를 검사하고 통과시킨다.
`a_git_path_argument_cannot_leave_the_workspace_through_a_link`가 이것을 잡았다. 이제
`InvalidArgument`와 `NotFound`에서만 넘어가고 거부는 거부로 전파한다.

**`sh`는 `PATH` 없이도 자기 기본 `PATH`를 만든다.** `the_environment_starts_empty`를 "자식의
`$PATH`가 비어 있다"로 쓸 수 없다 — POSIX 셸은 환경에 `PATH`가 없으면 스스로 하나를 만든다.
호스트에는 있고 셸이 발명하지 않는 이름(`$HOME`)이 비어 있는지, 그리고 자식의 `PATH`가
호스트의 것과 **다른지**로 다시 썼다. 관련해서 테스트는 `sh`를 `/bin/sh`로 절대 경로
지정한다 — 빈 환경에는 프로그램을 찾을 `PATH`가 없고, 그것이 이 절이 증명하려는 성질이다.

### 8.4 리뷰 4의 non-blocking

넷 다 반영했다. 본문은 승인된 상태 그대로 두었으므로 여기 적는다.

- `docs/plugin.md`의 "어느 프로파일도 주지 않는 permission이 셋" 블록을 고쳤다. §3.9의
  `docs/plugin.md` 행이 이 블록을 빠뜨렸고, 그 문단은 이 Phase가 끝나는 날 거짓이 된다.
- §6.0 A3의 관계 단언을 `BTreeSet` 비교로 썼다. 양변이 `Vec`이고 순서가 다르다.
- `every_capability_slot_is_readable`에 `sandbox(..)` 한 줄. Phase 4가 그 슬롯에 하중을
  거는데 여덟 슬롯 중 그것만 읽히지 않고 있었다.
- `interceptors_run_concurrently_not_serially`의 상한을 2.5 s → **3 s**. §6.4가 스스로 적은
  "최소 2배"보다 빡빡했고, 3 s도 직렬(4.5 s)과 충분히 구분된다.

리뷰 4가 판단을 요청한 둘(§7-10 `readonly`의 `ProcessSpawn`, §7-13 기억 조회 순서)은
리뷰어가 설계에 동의했으므로 설계대로 구현했고, 둘 다 이름 붙은 테스트가 붙들고 있다
(`a_readonly_profile_may_run_a_process_but_holds_no_write` ·
`a_remembered_approval_is_honored_even_when_unattended`).

### 8.5 빌드 리뷰가 찾은 것 — argv 주입

설계에도 없고 구현 중에도 보지 못한 것이다. 빌드 리뷰 1라운드의 blocking:

> `git_diff`의 `rev`가 검증 없이 argv로 들어가므로, `-`로 시작하는 값은 `git`이 **옵션**으로
> 읽는다. `rev = "--output=../../pwned"`는 `git`이 워크스페이스 밖에 파일을 쓰게 만든다 —
> `fs_write`를 전혀 주지 않는 프로파일에서.

리뷰어가 `git`의 동작을 실측했고, 나도 별도로 재현했다: `git --no-pager diff
--output=../ESCAPED`는 exit 0으로 끝나고 워킹 디렉터리 위에 105바이트를 쓴다. 완료 판정 B
(봉쇄)와 DoD 6을 동시에 깬다.

**고친 방식이 요점이다.** `rev` 한 줄에 검사를 더하는 것이 아니라, 규칙을 타입으로 옮겼다 —
`rivet_runtime::argv::Argv`(신규). `push`가 없고 문이 넷이며(§3.9의 `plugin.md` 4.5c에 표가
있다) 넷 다 각자의 방식으로 안전하다. `tool-git`과 `tool-shell`의 **모든** argv 항목이 그
넷 중 하나를 지난다.

이유는 리뷰어의 마무리가 정확히 짚었다: `path`를 안전하게 만든 규칙(`--` 구분자)이 중앙이
아니라 **호출 지점마다** 적용됐고, 그래서 `rev`가 빠졌다. 같은 모양의 수정은 다음 인자를
또 놓친다. 그리고 그것을 붙드는 테스트도 인자 이름을 나열하지 않는다 —
`no_declared_argument_can_become_an_option`이 `spec().input_schema`를 훑어 **선언된 모든
문자열 인자**에 옵션 모양의 값을 먹이므로, 나중에 더해지는 인자는 선언되는 날 자동으로
덮인다. `tool-shell`에도 같은 테스트가 있다.

`operand`의 거부는 `PolicyDenied`다 — 탈출하는 경로와 같은 종류라서, 디스패처가
`tool.blocked`으로 남긴다. 좁게 유지하는 것도 확인했다: `HEAD~1`·`main`·`origin/main` 같은
평범한 리비전은 그대로 돌고(`an_ordinary_revision_still_works`), `-m`에 묶인 커밋 메시지는
`--amend`로 시작해도 메시지다(`a_commit_message_may_still_begin_with_a_dash`) — `-m`이
그것을 그대로 삼키므로 애초에 위험하지 않았고, 거부했다면 위험하지 않은 것을 거부하는
셈이었다.

같은 리뷰의 non-blocking 여섯도 반영했다: C1(상속된 파이프)의 회귀 테스트
(`a_grandchild_holding_the_pipe_does_not_wedge_a_finished_call` — `settle`을 무제한 await로
되돌리면 5초를 기다리다 실패하는 것을 확인했다), `settle`이 리더 핸들을 `abort`하도록,
`tool.called`의 append를 scope 생성 **위로** 올려 teardown이 다른 모듈의 게으름이 아니라
함수의 모양으로 보장되도록, `an_approval_leaves_a_requested_and_a_resolved_in_the_log`가
requested 쪽도 단언하도록, 패닉 경로의 teardown 테스트 둘, 그리고
`a_run_publishes_every_agent_and_tool_topic_this_phase_owns`에 `tool.policy.evaluated`
추가(승인 쌍을 넣지 않은 이유는 그 자리에 문단으로 적었다).

### 8.6 §7의 열린 질문 중 이 Phase 밖으로 나간 것

셋을 [`architecture.md` §11](../architecture.md)로 승격했다. 나머지는 이 Phase 안에서
닫히거나 이 문서에 남는다.

- **§7-2 (provider 이름 사이에 순서가 없다)** → §11-9 옆이 아니라 이 문서에 남긴다. `docker`가
  오기 전에는 관측될 수 없고, 고치려면 `rivet-core` 계약이 바뀐다.
- **§7-3 (`SecretsRead`)** → §11-6에 "Phase 4가 범위 밖으로 두었다"와 그 이유를 적었다.
- **§7-14 (`ToolSpec`이 요구 permission을 선언하지 않는다)** → §11-13에 이어 붙였다. Phase 4는
  호출 시점의 grant 판정을 더했지만 그것이 보는 것은 `FsWrite`의 유무이지 scope가 아니다.
- **§7-15 (`§11-9` 재검토)** → §11-9에 "의도적으로 다시 열었고 물려받기로 했다"를 적었다.

그리고 이 Phase가 닫은 것: §11-7(비-UTF-8 출력), §11-10의 `ProcessSpawn`,
§11-14(spec 등록 시점 고정). §11-11(interceptor capability)은 닫지 않았지만 **기각 이유가
바뀌었다** — "Phase 2의 범위 밖"에서 "요청자가 없다"로. §11-15(`Plugin::load` 데드라인)는
그대로 열려 있고, 왜 같은 모양을 재사용하지 않았는지는 §5의 "알려진 미해결"과 §11-15 자신에
적혀 있다.

### 8.7 PR 리뷰 1라운드 (`f5933b8` 위)

PR #5에 붙은 독립 리뷰가 blocking 하나와 non-blocking 여덟(빌드 리뷰 2가 넘긴 여섯 + 리뷰어
자신의 둘)을 냈다. 전부 반영했다.

**blocking — 승인 sink가 화면보다 오래 산다.** `--tui`에서 `q`는 `Reaction::StopDrawing`이고,
이것은 렌더 루프를 끝내되 **런은 계속하게 둔다**(`quitting_leaves_the_ui_without_stopping_the_run`이
그 성질을 못 박는다). 그런데 `cfg.approval_sink`는 여전히 같은 `Arc<Tui>`를 쥐고 있었다.
렌더 루프가 없으면 키 경로가 없고, 키 경로가 없으면 `answer_approval`에 도달할 방법이 없다.
그래서 `q` 뒤에 승인이 필요한 호출이 하나라도 나오면 — `developer`의 `git_commit`(경로 인자가
없어 4번 규칙에 걸린다), 파괴적 형태에 걸리는 셸 명령 — `Tui::request`의 `receiver.await`에서
**`max_duration_ms`(기본 30분)** 동안 아무것도 그리지 않은 채 멈춘다. Ctrl-C로 회복되므로
교착은 아니지만, `--headless`와 `render/approve.rs`가 각자 막으려던 바로 그 실패이고 셋째
문으로 들어온 것이다. 두 기존 장치는 **런 시작 시점의** 상태만 본다.

고친 방식: "화면이 없다"를 sink가 아는 **상태**로 만들었다. `Tui`가 `dismissed:
CancellationToken`을 들고, `Tui::dismiss()`가 그리기를 끝내는 것과 sink를 닫는 것을 **한 사실**로
묶는다. `request`는 들어갈 때 확인하고(이미 사라진 화면에 질문을 올리지 않는다) 기다리는
동안에도 함께 경주한다(`q`가 도착했을 때 이미 화면에 있던 프롬프트도 풀린다). 반환은
`Err`이고 `Approvals::decide`가 이미 sink의 `Err`를 `Denied`로 접으므로 — 닫히는 방향이고
`--headless`와 같은 답이다 — 하류는 바꿀 것이 없다.

토큰을 하나로 만든 것이 요점이다. `render_loop`는 넘겨받은 토큰이 아니라 화면 자신의 것을
보고, `Screen`은 `stop` 토큰 대신 `Arc<Tui>`를 들며, `Screen::stop`·`Screen::drop`·
`Reaction::StopDrawing` 셋 다 `dismiss()`를 부른다. `drive`는 **자기가 끝나는 모든 경로에서**
(터미널이 죽어 `?`로 나가는 것 포함) `Dismissal` 가드로 dismiss한다. 그래서 "화면이 없으면
sink도 없다"가 루프의 성질이지 루프를 시작한 사람이 기억해야 하는 규칙이 아니다. 그리고
sink 자체를 `Screen::start` **뒤에서** 만들어, 화면이 시작되지 못한 경우에는 `--tui`도 sink를
갖지 않는다.

테스트 넷: `a_dismissed_screen_is_not_a_sink` ·
`dismissing_releases_an_approval_that_was_already_waiting` ·
`a_render_loop_that_ends_on_its_own_stops_being_a_sink` ·
`quitting_also_stops_the_screen_from_answering_approvals`(호스트 쪽 배선). 앞의 둘은
`tokio::time::timeout` 안에서 돈다 — 회귀는 *멈춤*이므로, 상한이 없으면 실패하는 대신 스위트를
매달리게 한다. 수정 전 코드로 되돌려 확인했다: 호스트 쪽 배선을 빼면 CLI 테스트가 실패하고,
sink 쪽 확인을 빼면 TUI 스위트가 매달린다.

**non-blocking 여덟.**

1. `argv.rs:77` + `argv.rs:174` — 리뷰어 말대로 **한 편집으로** 닫았다. `Argv::option`의
   플래그가 `&'static str`이 아니라 `Consuming`(비공개 필드, `argv.rs`에 선언된 `const` 셋)이
   되어, "이 옵션이 뒤 항목을 실제로 삼킨다"가 호출 지점의 판단이 아니라 타입의 사실이 됐다.
   같은 목록을 `option_exposure`가 binder 규칙으로 쓰므로, 스키마를 훑는 일반 순회가
   `option("--staged", 모델값)` 오용을 **이제 볼 수 있다**(`the_oracle_binds_to_the_declared_options_and_to_nothing_else`가
   양쪽을 단언한다). 값 붙임(`--flag=value`)은 택하지 않았다 — `sh -c<command>`가 `/bin/sh`
   구현마다 보장되지 않고 이 모듈은 어떤 프로그램에 대해서도 성립해야 한다. 공유 술어의
   나머지 절반은 `operand`가 `value.starts_with('-')`를 **직접** 쓰게 해서 닫았다: 가드를
   변이시켜도 oracle이 함께 멀지 않는다.
2. `tool-git`·`tool-shell`의 순회가 `type == "string"`만 걸렀다 — 문자열을 **담을 수 없는**
   셋(`boolean`·`integer`·`number`)만 제외하도록 뒤집고, `array`에는 한 원소 배열을 먹이며,
   마지막 단언을 **선언된 전체 이름 집합**으로 넓혔다. 어떤 타입의 인자가 새로 생겨도 이
   파일을 한 번은 보게 된다.
3. `tool-git/src/tools.rs:107` — `path: "."`가 빈 pathspec을 만들어 `git`이 exit 128로
   "empty string is not a valid pathspec. please use . instead"라고 답한다. 그 안내가 가리키는
   입력이 방금 실패를 만든 그 입력이라 모델이 루프에 빠진다. 두 호출 지점이 아니라
   `Argv::pathspec` 한 문에서 빈 문자열을 버린다 — 빈 pathspec은 "전부"이고 아무것도
   내보내지 않는 것과 같은 뜻이다.
4. `dispatch.rs:563` — 도구가 **자기 안에서** 올린 `PolicyDenied`가 감사 로그에
   `policy: "workspace"`로 남았다. `default.workspace`는 그 호출을 평가한 적이 없다. 리뷰어의
   (a)안을 택해 `WORKSPACE_POLICY` → `TOOL_POLICY = "tool"`("도구 자신의 봉쇄가 거절했다")로
   바꿨다 — §3.2와 §6.0이 `WORKSPACE_POLICY`라는 이름으로 부르는 상수가 이것이고, 남긴다는
   결정은 그대로이며 바뀐 것은 그 상수가 담는 **문자열**뿐이다.
   `a_refusal_the_tool_raised_is_filed_under_the_tool_and_not_a_policy`가 리터럴로 못 박는다. 함께: 호스트가 쓰는 정책 이름 셋(`host.baseline` · `agent.scope` · `tool`)을
   `HOST_POLICY_NAMES`로 모아 `register_policy`가 전부 예약한다 — 전에는 `host.baseline`
   하나뿐이었다.
5. `app.rs:257-263` — `state.pending`과 `self.answer`를 순차로 잡던 두 뮤텍스를, `request`와
   `answer_approval` 양쪽에서 **함께, 같은 순서로**(`state` → `answer`) 잡는다. blocking과
   같은 impl에 있으므로 같은 패스에서 고쳤다.
6. `sandbox_scope.rs:166`(리뷰어 자신의 발견) — `provider.prepare()`를 `self.state` 락을 쥔 채
   await했고 `teardown()`이 같은 락을 잡는다. 자식에 대해서는 성립하던 모듈의 중심 주장이
   prepare에 대해서는 성립하지 않았다. `Preparing` 상태를 넣어 락을 놓고 준비하고 다시 잡아
   `TornDown`을 재확인한다 — 그 사이에 봉인됐으면 방금 만든 핸들을 **스스로 teardown**한다.
   동시 `exec` 둘이 환경 둘을 만들지 않도록 `Notify`로 기다린다. 오늘 깨지는 것은 없었지만
   (`LocalSandbox::prepare`에 await가 없다) "모든 경로에서 teardown"이 다른 모듈에 대한
   사실이 아니라 모양의 성질이어야 한다는 것이 이 설계의 논지였다.
   `teardown_does_not_wait_for_a_preparation_in_flight`가 붙들고, 락을 다시 쥐게 되돌리면
   5초 타임아웃으로 실패하는 것을 확인했다.
7. `destructive.rs:114`(리뷰어 자신의 발견, 판단 요청) — **`true`가 맞다고 판단했고, 그 이유를
   `true` 옆에 적었다.** 기억되는 단위가 *도구*이고 도구의 도달 범위는 자기 스키마와 봉쇄가
   정하므로 `a`는 사람이 한 번 보고 뜻할 수 있는 말이다. 기본값 `production`은
   `process_spawn`도 `fs_write`도 주지 않으므로 그 아래에서 닿는 것 중 열린 도달 범위를 가진
   것이 없다. **다만 하나를 닫았다**: 셸 형태 규칙을 프로파일 규칙 **앞으로** 옮겼다. 전에는
   `require_approval_for_all_in`에 셸을 가진 프로파일을 넣는 것만으로, 파괴적 셸 명령이
   `scope_key = "shell"` · `allow_remember: true`로 답해져 "셸 게이트는 절대 기억되지 않는다"가
   한 번의 `a`로 뒤집혔다. 이제 그런 호출은 언제나 셸 규칙이 답한다
   (`a_profile_that_approves_everything_cannot_remember_a_shell_gate`).
8. 리뷰어가 확인해 준 사실 하나는 코드 변경이 아니다. **ubuntu CI가 이 커밋에서 이미
   초록이다** — run `34199445217`(head `f5933b8`)이 `ubuntu-latest`에서 fmt · clippy ·
   `cargo test --workspace --all-features` · `RUSTDOCFLAGS="-D warnings" cargo doc`를 돌리고,
   MSRV 1.90 `cargo check`와 `cargo audit`도 함께 돈다. 즉 §6.4가 요구한 "CI(ubuntu)와 개발
   머신(darwin) 양쪽"은 **충족돼 있고**, 타이밍에 의존하는 셋도 리눅스에서 돌았다. 저장소
   안에는 이와 어긋나는 문장이 없다(§6.4의 문장은 요구사항이지 측정 결과의 주장이 아니다).

게이트 재측정: `cargo fmt --all -- --check` 차이 없음 · `cargo clippy --workspace
--all-targets -- -D warnings` 경고 0 · `cargo test --workspace` **772 passed · 0 failed ·
1 ignored**(기준선 760에서 +12; 사라진 테스트 없음 — 이름 집합을 비교했고 유일한 차이는
`no_registered_policy_may_claim_the_baseline_name`이 셋을 다 도는
`no_registered_policy_may_claim_a_name_the_host_writes`로 이름이 바뀐 것이다) ·
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` 경고 0.
