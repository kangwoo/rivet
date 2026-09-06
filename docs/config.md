# 설정 가이드

> 대상: `rivet.toml`을 쓰는 사람 · 기준: Phase 1

이 문서는 **지금 동작하는 것**과 **파싱만 되고 아직 아무 일도 하지 않는 것**을 구분해서
적는다. 후자를 조용히 빼놓으면 "설정했는데 왜 안 되지"가 되고, 적지 않으면 예제 파일에
있는 섹션이 뭔지 알 길이 없다.

전체 예시는 [`rivet.example.toml`](../rivet.example.toml)에 있다. 복사해서 시작하면 된다.

```bash
cp rivet.example.toml rivet.toml
```

`rivet.toml`은 `.gitignore`에 있다. 머신마다 다른 설정이고, 추적되는 템플릿은
`rivet.example.toml` 쪽이다.

---

## 1. 파일을 어디서 찾는가

먼저 찾은 것 하나만 쓴다. 병합하지 않는다.

| 순서 | 출처 |
|---|---|
| 1 | `--config <경로>` |
| 2 | 환경 변수 `RIVET_CONFIG` |
| 3 | 현재 디렉터리부터 위로 올라가며 만나는 첫 `rivet.toml` |
| 4 | 없으면 **전부 기본값** — 설정 파일 없이도 돈다 |

1번과 2번은 가리킨 파일이 없으면 **실패한다.** 조용히 3번으로 넘어가지 않는다. 명시적으로
지목한 파일이 없다는 건 오타이지 "기본값으로 하라"는 뜻이 아니기 때문이다.

## 2. 워크스페이스 루트는 어디인가

**설정 파일이 들어 있는 디렉터리**다. git 루트가 아니다.

- Rivet은 저장소가 아닌 디렉터리에서도 동작해야 한다.
- git 루트는 설정 작성자가 의도한 범위보다 훨씬 넓을 수 있다.

"규칙이 놓인 곳"이 "규칙이 다스리는 곳"이라는 게 가장 예측 가능한 해석이다.
`[workspace] root`로 명시하면 그쪽이 이긴다. 설정 파일 자체가 없으면 현재 디렉터리다.

세션 로그는 워크스페이스 루트 밑 **`.rivet/sessions`** 에 쌓인다 (`.gitignore`에 이미 있다).

---

## 3. 섹션 레퍼런스

### `[agent]`

```toml
[agent]
model = "deepseek/deepseek-chat"
instructions = ""   # 예제 파일에는 없다
```

| 키 | 기본값 | 설명 |
|---|---|---|
| `model` | `"deepseek/deepseek-chat"` | `provider/모델명`. **첫 `/` 앞은 어댑터 라우팅용이라 잘려나가고 뒤가 wire에 나간다.** `openrouter/meta-llama/llama-3-70b` → `meta-llama/llama-3-70b` |
| `instructions` | `""` | 시스템 프롬프트에 덧붙일 문장. 비우면 런타임 기본 서문만 나간다 |

### `[agent.limits]` — 하드 스톱 5축

```toml
[agent.limits]
max_turns                   = 50
max_duration_ms             = 1_800_000
max_total_tokens            = 2_000_000
max_context_tokens          = 128_000
max_consecutive_tool_errors = 5
```

키 이름은 `RunLimits`의 필드명과 **접두사까지 정확히 같다.** 설정 키가 자기가 설정하는
계약과 이름이 다르면 그것 자체가 함정이라서다.

각 축은 발동할 때 어느 축인지 (`LimitKind`) 를 종료 사유에 남기고, 프로세스는 종료 코드
3으로 끝난다. 다섯 축 전부 개별 테스트가 있다.

- `max_context_tokens`는 조립된 요청에 대해 측정된다. Phase 1은 tool schema 비용까지
  포함해서 예산을 잡으므로, "조립기는 들어간다고 했는데 루프가 거부"하는 일은 없다.

### `[workspace]`

```toml
[workspace]
deny = [".env", ".git/config", ".ssh", "**/credentials.json", "**/*.pem"]
root = "..."   # 예제 파일에는 없다
```

`deny`는 **워크스페이스 안에 있지만 읽지도 쓰지도 않을 경로**다.

- 워크스페이스 상대 경로에 대한 글롭, **대소문자 무시**
- 맨 이름은 **모든 깊이에 적용**된다 — `.env`는 `sub/.env`도 막는다
- 디렉터리를 적으면 그 안쪽도 막힌다 — `.ssh`는 `.ssh/id_rsa`도 막는다
- **잘못된 패턴은 시작할 때 실패한다.** 아무것도 안 막으면서 막는 척하지 않는다

`.git/config`가 목록에 있는 이유는 `core.fsmonitor`와 `core.pager`가 임의 코드 실행
경로이기 때문이다. **git config에 쓸 수 있으면 셸을 얻은 것과 같다.**

> deny 목록은 어휘적 검사가 아니라 실제 파일 접근 시점에 강제된다. 워크스페이스 안의
> 심볼릭 링크가 밖을 가리키는 경우는 `fsguard`가 open 후 재검증해서 막는다
> (unix 기준, windows 미검증).

### `[plugins]`

```toml
[plugins]
enabled = ["rivet.model-openai", "rivet.tool-filesystem"]
```

Phase 1이 **실제로 등록할 수 있는** id는 셋뿐이다.

| id | 내용 |
|---|---|
| `rivet.model-openai` | OpenAI 호환 provider |
| `rivet.tool-filesystem` | `read_file` `write_file` `list_dir` `search` |
| `rivet.context-builtin` | 시스템 프롬프트 + 워크스페이스 provider — **끌 수 없다.** 적어도 되고 적어도 아무 일 안 한다 |

예제 파일이 켜 두는 나머지 넷(`rivet.tool-shell`, `rivet.tool-git`,
`rivet.policy-default`, `rivet.sandbox-local`)은 **경고하고 건너뛴다.**

```
· `rivet.tool-shell` is enabled but ships in a later phase; skipping
```

예제를 복사한 설정이 첫 설정일 가능성이 가장 높아서, 실패시키지 않고 넘어간다.
**두 목록 어디에도 없는 id는 오타이고 시작할 때 실패한다.** Phase 2의 로더가 들어오면 이
중간 범주는 사라지고, 모든 id는 로드되거나 오타이거나 둘 중 하나가 된다.

### `[plugins."<plugin id>"]` — plugin별 설정

plugin id를 키로 쓰는 테이블이 그 plugin에 그대로 전달된다.

```toml
[plugins."rivet.model-openai"]
base_url    = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"
```

`rivet.model-openai`가 받는 것 (전부 선택, 아래는 기본값):

| 키 | 기본값 |
|---|---|
| `base_url` | `"https://api.openai.com/v1"` — `/chat/completions` **앞까지** |
| `api_key_env` | `"OPENAI_API_KEY"` |
| `include_usage` | `true` — 알 수 없는 필드를 거부하는 엔드포인트면 끈다 |
| `send_reasoning` | `false` |
| `request_timeout_ms` | `600_000` — 스트리밍 본문 포함. 무한정 매달릴 수 있는 어댑터는 `max_duration_ms`를 권고사항으로 만든다 |
| `connect_timeout_ms` | `15_000` |
| `context_window` | 없음 |
| `max_output_tokens` | 없음 |
| `supports_tools` | `true` |
| `supports_vision` / `supports_reasoning` / `supports_prompt_caching` | `false` |

**API 키는 이 파일에 넣지 않는다.** `api_key_env`는 키가 들어 있는 **환경 변수의 이름**을
가리킨다. 키 자체는 설정 파일에도, 에러 메시지에도 등장하지 않는다.

provider별 조합:

```toml
# OpenAI
[agent]
model = "openai/gpt-4o-mini"
[plugins."rivet.model-openai"]
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

# Ollama (로컬)
[agent]
model = "ollama/qwen2.5-coder"
[plugins."rivet.model-openai"]
base_url = "http://localhost:11434/v1"
api_key_env = "OLLAMA_API_KEY"
```

`[agent] model`과 `[plugins."rivet.model-openai"]`는 **같이 바꿔야 한다.** 하나만 바꾸면
엉뚱한 엔드포인트에 엉뚱한 모델명을 보낸다.

### `[policy]` — 프로파일

```toml
[policy]
profile = "developer"   # developer | readonly | reviewer | ci | production
```

**Phase 1에서 프로파일이 하는 일은 에이전트의 도구 범위를 좁히는 것뿐이다.** 파이프라인
2단계이지 정책 집행이 아니다. 정책 체인·승인·샌드박스는 Phase 4다.

그래도 하는 일은 실재한다 — **등록되지 않은 도구는 모델에게 제시되지 않고, 제시되지 않은
도구는 호출될 수 없다.** `readonly`는 모델이 `write_file`을 애초에 받지 못한다는 뜻이지,
정책이 막아준다는 뜻은 아직 아니다.

| 프로파일 | 쓰기 도구 | 도구 범위 | 무인 |
|---|---|---|---|
| `developer` | ○ | 전부 | |
| `ci` | ○ | 전부 | ○ |
| `readonly` | ✗ | 전부(쓰기 제외) | |
| `production` | ✗ | 전부(쓰기 제외) | |
| `reviewer` | ✗ | `read_file` `list_dir` `search` 만 | |

`ci`만 무인으로 친다. `production`을 무인으로 두면, Phase 4에서 승인이 붙는 순간 모든
승인 대상을 **묻지 않고 자동 거부**하게 된다 — 보안 문서의 프로파일 표는 `production`을
"전부 승인 필요" 칸에 두고 있으므로 그건 틀린 동작이다. CI에서는 `--headless`와 `ci`를
같이 쓴다.

**모르는 프로파일 이름은 시작할 때 실패한다.** 관대한 기본값으로 조용히 떨어지지 않는다.
오타가 보안 사고가 되는 경로가 그것이다.

### 아직 동작하지 않는 섹션

파싱은 되고 `rivet doctor`가 보고하지만, **아무 일도 하지 않는다.** 무시하지 않고 알려주는
쪽을 택했다 — 샌드박스를 설정한 사람은 그게 아직 아무것도 강제하지 않는다는 걸 알아야 한다.

| 섹션 | 언제 살아나는가 |
|---|---|
| `[sandbox]` | Phase 4 |
| `[job]` | Phase 5 |
| `[agents.<이름>]` | 이름 붙은 에이전트를 고를 CLI 표면이 아직 없다 |

`[agents.reviewer]`의 `context_providers`가 부르는 `job`·`git` provider는 나중 단계에
생긴다. Phase 1이 등록하는 건 `system`과 `workspace` 둘이다.

---

## 4. CLI 플래그

전역 플래그이고, 설정 파일보다 **우선한다.**

| 플래그 | 설명 |
|---|---|
| `--config <경로>` | 설정 파일 지정 |
| `--profile <이름>` | `[policy] profile` 덮어쓰기 |
| `--headless` | TUI도 승인 프롬프트도 없이 실행. 사람에게 물어야 하는 건 매달리지 않고 **거부**된다. CI용 |
| `--jsonl` | 렌더링 대신 줄 단위 JSON 이벤트를 stdout으로. `--headless`와 함께 쓸 수 없다 |

`--jsonl`은 **관찰용이지 세션 재구성용이 아니다.** 이벤트 버스는 설계상 lossy하다.
세션을 재구성하려면 `rivet session show --json`을 쓴다.

### 종료 코드

스크립트가 프로즈를 파싱하지 않고 분기할 수 있어야 한다.

| 코드 | 뜻 |
|---|---|
| 0 | 정상 종료 |
| 1 | 런 실패 또는 런타임 실패 |
| 2 | 설정 문제 — 파일이 잘못됐거나, 프로파일이 없거나, 자격 증명이 없거나 |
| 3 | 한도 발동 |
| 4 | 정책 거부 (Phase 4부터 실제로 발생) |
| 130 | 취소 — 관례대로 `128 + SIGINT` |

---

## 5. 환경 변수

| 변수 | 용도 |
|---|---|
| `RIVET_CONFIG` | 설정 파일 경로 |
| `api_key_env`가 가리키는 변수 | provider API 키. 기본 이름은 `OPENAI_API_KEY` |
| `RUST_LOG` | 로그 필터. 기본 `warn`, stderr로 나간다 |
| `RIVET_DUMP_REQUESTS` | 조립된 요청을 이 디렉터리에 덤프한다 (디버깅용) |

테스트에서만 쓰는 것: `RIVET_LIVE`, `RIVET_LIVE_BASE_URL`, `RIVET_LIVE_KEY_ENV`,
`RIVET_LIVE_MODEL` (실제 provider 왕복 테스트), `RIVET_RECORD_FIXTURES` (SSE 픽스처 녹화).

---

## 6. 설정이 맞는지 확인하기

```bash
rivet doctor
```

출력은 다섯 덩어리다.

| 덩어리 | 내용 |
|---|---|
| `configuration` | 설정 파일 경로, 워크스페이스 루트, 세션 디렉터리, 모델, 프로파일, 무인 여부 |
| `limits` | 한도 5축의 실제 적용값 |
| `deny list` | 패턴 목록 + **글롭을 실제 경로에 넣어 돌려본 결과** (`probe .env  blocked`) |
| `configured but not yet in force` | 파싱됐지만 아직 동작하지 않는 섹션 |
| `credentials` / `plugins` | `api_key_env`가 가리키는 변수가 실제로 있는지, plugin이 뜨는지 |

deny 패턴은 컴파일해서 실제로 물어보는 게 핵심이다 — 목록에 있다는 것과 실제로 막힌다는
것은 다른 이야기이고, `.ssh`가 `.ssh/id_rsa`를 막는지는 눈으로 봐서는 알 수 없다.

```
deny list
  .env
  .git/config
  ...
  probe .env                 blocked
  probe sub/.env             blocked
  probe .git/config          blocked
  probe keys/server.pem      blocked

configured but not yet in force
  [sandbox]   read, but no confinement is applied until Phase 4
  [job]      read, but the job runtime lands in Phase 5
  [agents.*]  reviewer declared; Phase 1 has no way to select one

credentials
  the environment variable `OPENAI_API_KEY` is not set; it must hold the API key
  for `openai/gpt-4o-mini`. Set it, or point `api_key_env` at a different variable
```

**종료 코드로 판정한다** — 문제가 있으면 2로 끝나므로 CI에서 그대로 게이트로 쓸 수 있다.
빈 `deny` 목록도 여기서 걸린다.

```bash
rivet doctor || echo "설정을 고쳐야 한다"
```

---

## 7. 최소 설정

설정 파일 없이도 돈다. 기본값이 전부 채워지고, 워크스페이스는 현재 디렉터리다.
그래도 최소한 이 정도는 적어 두는 편이 낫다.

```toml
[agent]
model = "openai/gpt-4o-mini"

[workspace]
deny = [".env", ".git/config", ".ssh", "**/credentials.json", "**/*.pem"]

[plugins]
enabled = ["rivet.model-openai", "rivet.tool-filesystem"]

[plugins."rivet.model-openai"]
base_url    = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
```

`deny`를 비우면 워크스페이스 안의 **어떤 것도 보호되지 않는다.** `rivet doctor`가
`(empty) — nothing inside the workspace is protected`라고 말해 준다.
