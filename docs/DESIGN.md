# agent-crew — 멀티 에이전트 협업 오케스트레이터 (macOS / Rust)

> 작업명 `agent-crew` (가칭). 2026-08-27 초안.
> 목표: 구독 중인 여러 AI CLI(클로드 코드/코덱스/그록/제미나이…)를 **하나의 팀**으로 묶어,
> 사용자의 요청 한 줄을 팀장 에이전트가 스프린트로 쪼개고, 역할별 에이전트가 서로 대화하며
> 끝까지 완주시키는 macOS 앱.

---

## 0. 먼저 짚을 것 — 버즈와 우리가 만들 것은 다르다

| | 버즈(Buzz) | agent-crew |
|---|---|---|
| 구조 | **수평**. 사람이 각 에이전트에게 지시 | **수직**. 팀장 에이전트가 분배·조율 |
| 진행 | 사람이 다음 단계를 밀어야 함 | DAG + 수락기준으로 자동 진행 |
| 협업 | 스레드 공유 = 사람이 읽고 이어붙임 | 에이전트→에이전트 **수정 요청/재작업 루프** |
| 컨텍스트 | 스레드 전체가 곧 컨텍스트 | 계층 압축(공유사실/태스크브리프/개인세션) |
| 신뢰 | 서명·릴레이(멀티 유저 전제) | 로컬 단일 사용자 → 서명은 3단계로 미룸 |

영상에서 "썸네일 제너레이터가 태그를 안 받아서 반응 안 함"이 나온 그 지점이,
버즈가 **메신저**이지 **오케스트레이터**가 아니라서 생기는 구조적 공백입니다.
우리가 만들 것의 핵심 가치는 바로 그 공백 — **디스패치·의존성·수락(DoD) 루프**입니다.

---

## 1. 아키텍처 개요

```
┌─────────────────────────────────────────────────────────────┐
│  UI (Tauri WebView)  스프린트 보드 · 라이브 스레드 · DAG · 승인함 │
└───────────────▲─────────────────────────────────────────────┘
                │ Tauri IPC (event stream)
┌───────────────┴─────────────────────────────────────────────┐
│  crew-core (Rust)                                            │
│   ├ Orchestrator   : 스펙→DAG→스프린트→디스패치→수락           │
│   ├ MessageBus     : 라우팅/상관관계/타임아웃/재시도            │
│   ├ ContextStore   : 이벤트 원장 + 계층 컨텍스트 조립           │
│   └ WorkspaceMgr   : git worktree 할당·머지·충돌 감시           │
└───────────────▲─────────────────────────────────────────────┘
                │ WebSocket (ws://127.0.0.1:PORT, 토큰 인증)
    ┌───────────┼───────────┬───────────┬───────────┐
    │           │           │           │           │
┌───┴───┐  ┌────┴───┐  ┌────┴───┐  ┌────┴───┐  ┌───┴────┐
│ Lead  │  │  PM    │  │ Design │  │  Dev   │  │  QA    │   ← crew-agent 프로세스
└───┬───┘  └────┬───┘  └────┬───┘  └────┬───┘  └───┬────┘
    │ stdio(stream-json) 상시 세션
┌───┴──────────────────────────────────────────────────────┐
│ claude / codex / gemini / grok / opencode  (구독 계정 그대로) │
└──────────────────────────────────────────────────────────┘
```

**왜 WebSocket인가 (요구사항이기도 하지만 실제로 정당함)**
- 에이전트 러너를 **별도 프로세스**로 두면: CLI가 죽어도 코어가 안 죽고(크래시 격리),
  에이전트별 워크트리/작업 디렉토리를 프로세스 단위로 분리할 수 있고,
  나중에 원격 머신·다른 사람의 맥으로 확장할 때 코드 변경이 0에 수렴합니다.
- 단, **코어 내부**까지 WS로 하면 낭비입니다. 내부는 `tokio::mpsc` 액터, WS는 **프로세스 경계**에만.

---

## 2. 구독 에이전트를 붙이는 법 (이 프로젝트의 심장)

> 외부 근거 대조: [§13.1](#131-2-구독-에이전트를-붙이는-법과의-대조), [docs/RESEARCH.md](RESEARCH.md) §3·§4 참고.

### 2.1 검증된 사실 (이 맥북에서 확인)
```
claude -p --input-format stream-json --output-format stream-json \
       --session-id <uuid> --include-partial-messages
```
- `--input-format stream-json` = **실시간 스트리밍 입력**. 즉 프로세스를 띄워두고 계속 말을 걸 수 있음
  → 매번 새 프로세스를 띄우는 방식 대비 결정적 우위.
- `-r/--resume <session_id>`, `--fork-session` 으로 세션 복원/분기 가능.
- `--agents <json>` 으로 역할별 시스템 프롬프트 주입 가능.

### 2.2 반드시 지켜야 할 두 가지 (실측 기반)
1. **`claude -p`는 실패해도 `subtype:"success"`를 뱉는다.** 성공/실패 판정은 `is_error` 와
   `terminal_reason` 으로 해야 합니다. 이걸 잘못 보면 오케스트레이터가 실패한 태스크를
   "완료"로 올리고 다음 스프린트로 넘어갑니다 → **가장 위험한 실패 모드**.
2. **호출당 시스템 프리앰블이 40~52k 토큰.** 태스크마다 프로세스를 새로 띄우면
   토큰이 프리앰블로 증발합니다. → 에이전트당 **롱리브드 세션 1개**를 원칙으로,
   스프린트 경계에서만 압축 후 재시작.

### 2.3 어댑터 계약
```rust
#[async_trait]
pub trait Harness: Send + Sync {
    fn id(&self) -> HarnessId;                     // claude-code / codex / grok / opencode ...
    async fn spawn(&self, cfg: &AgentCfg) -> Result<Session>;
    async fn send(&self, s: &mut Session, turn: UserTurn) -> Result<()>;
    fn events(&self, s: &Session) -> BoxStream<'_, HarnessEvent>; // token / tool_use / result / error
    async fn snapshot(&self, s: &Session) -> Result<HandoffPack>; // 하네스 교체용 컨텍스트 팩
    async fn shutdown(&self, s: Session) -> Result<()>;
}
```
`HarnessEvent` 는 CLI별 JSON을 **공통 이벤트**로 정규화합니다:
`Started / Thinking / ToolUse{name,input} / Text{delta} / Usage{in,out} / Finished{ok,reason} / Failed{err}`

- 1순위 구현: `claude-code` (설치·검증 완료)
- 2순위: `opencode` (설치됨), ACP 기반 어댑터 (Zed의 agent-client-protocol — 버즈가 쓰는 방식)
  (M8: 실 어댑터는 `PiHarness`(pi v0.75.5, RPC 모드)로 대체 구현됨 — pi 하나가 다중
  프로바이더/모델을 커버해 우선 채택. opencode는 스텁 존치, 삭제 안 함.)
- 미설치 CLI는 **설치 감지 후 비활성 카드**로 UI에 표시. 스텁만 두고 나중에.


### 2.4 팀 구성(로스터) — 하네스는 "팀원 슬롯의 설정값"이다

핵심 원칙: **역할(role)과 하네스(harness)는 완전히 분리**한다.
사용자는 팀원 슬롯을 만들고, 그 슬롯에 자기가 가진 것을 꽂는다.

```jsonc
// .crew/roster.json
{ "agents": [
  { "id":"lead",      "role":"lead",      "harness":"claude-code", "model":"opus",   "instructions":"..." },
  { "id":"pm",        "role":"planner",   "harness":"claude-code", "model":"sonnet", "instructions":"..." },
  { "id":"designer",  "role":"designer",  "harness":"claude-code", "model":"sonnet", "instructions":"..." },
  { "id":"publisher", "role":"publisher", "harness":"opencode",    "model":"...",    "instructions":"..." },
  { "id":"dev",       "role":"developer", "harness":"claude-code", "model":"opus",   "instructions":"..." },
  { "id":"qa",        "role":"qa",        "harness":"ollama",      "model":"qwen3",  "instructions":"..." }
]}
```

- **클로드 하나만 있어도 팀이 성립한다.** 5개 슬롯 전부 `claude-code` = 서로 다른
  세션·시스템 프롬프트·워크트리를 가진 5명의 팀원. 이게 기본값이자 MVP 시나리오.
- 하네스 레지스트리는 **설치된 것을 자동 탐지**해서 UI 드롭다운에 채운다
  (`which claude/codex/gemini/grok/opencode`, `ollama list`, LM Studio 로컬 엔드포인트).
- 슬롯의 하네스는 **런 도중에도 교체 가능** — 교체 시 §5의 핸드오프 팩이 자동 주입된다.
  (영상에서 말한 "컨텍스트 유지한 채 LLM 스위칭"이 여기서 나온다)
- 로스터는 **프리셋으로 저장**한다: `웹앱 5인팀`, `리서치 3인팀`, `혼자 개발 2인팀`.

**따라오는 제약 — 같은 구독의 동시 세션**
5개 슬롯이 전부 같은 클로드 계정이면 레이트리밋을 공유한다. 그래서 코어에
**하네스별 동시성 세마포어 + 대기 큐 + 백오프**가 필요하다(예: `claude-code: max 3 concurrent`).
Lead의 DAG 병렬도는 이 세마포어에 의해 자동으로 제한된다.

---

## 3. 메시지 프로토콜 (에이전트 ↔ 에이전트)

> 외부 근거 대조: [§13.2](#132-3-메시지-프로토콜과의-대조), [docs/RESEARCH.md](RESEARCH.md) §1·§2 참고.

### 3.1 봉투
```jsonc
{
  "id": "msg_01H...",            // ULID
  "ts": "2026-08-27T10:00:00Z",
  "sprint": "sp-3",
  "thread": "th-login-ui",       // 대화 스레드(사람이 UI에서 보는 단위)
  "from": "agent:designer",
  "to":   ["agent:publisher"],   // 브로드캐스트는 "channel:sprint-3"
  "kind": "change_request",
  "in_reply_to": "msg_01H...",   // 상관관계
  "corr": "req_01H...",          // 요청-응답 짝
  "body": { /* kind별 스키마 */ },
  "artifacts": ["art:design/login-v2.png"],
  "requires_ack": true,
  "deadline_ms": 900000
}
```

### 3.2 메시지 종류 (상태 전이 포함)
| kind | 방향 | 의미 | 수신자 의무 |
|---|---|---|---|
| `task.assign` | Lead→Agent | 태스크 배정 (브리프+DoD 포함) | `task.ack` 필수 |
| `task.ack` | Agent→Lead | **"받았습니다" 표시** (UI에 즉시 반영) | — |
| `task.progress` | Agent→* | 진행률/현재 단계/경과시간 | — |
| `task.result` | Agent→Lead | 산출물 + 자기점검 결과 | Lead가 검수 |
| `review.request` | Agent→Agent | "이거 확인해줘" | `task.ack` 후 검토 |
| `change_request` | Agent→Agent | **"기획대로 안 됐다, X 수정해줘"** | ack → 재작업 → `task.result` |
| `question` / `answer` | Agent↔Agent | 막힌 지점 질의 | 답변 or 15분 내 Lead 에스컬레이션 |
| `blocked` | Agent→Lead | 스스로 못 푸는 의존성 | Lead 재계획 |
| `handoff` | Lead→Agent | 사용량 소진 → 다른 하네스로 이관 | 컨텍스트 팩 수령 |
| `human.gate` | Lead→UI | 사람 승인 필요 | 사용자 클릭 |

### 3.3 요청하신 협업 시퀀스 그대로
```
PM ──review.request(스펙 v1 대비 디자인 검수)──▶ Designer
Designer ──task.ack (UI: "👀 확인함")───────────▶ PM
Designer ──change_request 없음 / 있음 판단
PM ──change_request{ "위반": ["빈 상태 화면 누락"], "근거": "spec§3.2" }──▶ Designer
Designer ──task.ack──▶ PM        (UI: 접수 뱃지)
Designer ──task.progress(2회)──▶ PM
Designer ──task.result{art: design/login-v3.png, diff: "..."} ──▶ PM
PM ── 검증 → accept / 2차 change_request (라운드 예산 차감)
```

### 3.4 무한 핑퐁 방지 (필수)
- `corr` 하나당 **최대 왕복 3회** → 초과 시 자동 `blocked` → Lead 중재.
- Lead 중재도 2회 실패 → `human.gate` 로 사용자에게 올림.
- 모든 메시지에 `deadline_ms`. 타임아웃 = 실패로 간주하고 Lead가 재배정.
- **A→B→A 순환 감지**: 동일 `corr` 그래프에 사이클 생기면 즉시 차단.

---

## 4. 오케스트레이션 모델 (팀장 에이전트)

> 외부 근거 대조: [§13.3](#133-4-오케스트레이션-모델과의-대조), [docs/RESEARCH.md](RESEARCH.md) §1·§2 참고.

### 4.1 요청 한 줄 → 실행까지
```
사용자: "회원가입/로그인 되는 랜딩 페이지 만들어줘"
   │
   ├ ① Lead: 스펙화(Spec)      — 목표/비목표/제약/수락기준. 모호하면 사용자에게 1회만 질문
   ├ ② Lead: 태스크 DAG 생성    — 역할·의존성·산출물 계약·DoD
   ├ ③ Lead: 스프린트 슬라이싱  — 컨텍스트 한계 기준으로 3~7 태스크씩
   ├ ④ 디스패치               — 의존성 풀린 태스크 병렬 실행
   ├ ⑤ 에이전트 간 자율 협업   — change_request 루프
   ├ ⑥ 스프린트 종료 게이트    — QA 통과 + Lead 수락 + 컨텍스트 압축
   └ ⑦ 다음 스프린트 or 완료
```

### 4.2 역할과 산출물 계약 (Artifact Contract)
계약이 없으면 "디자인이 기획대로 됐는지" 기계적으로 검증할 수 없습니다.

| 역할 | 입력 | 산출물 | 다음 단계가 검증할 수 있는 형태 |
|---|---|---|---|
| 기획자 PM | 사용자 요청 | `spec.md` (요구사항에 `REQ-1` 식 ID 부여) | ID 목록 = 체크리스트 |
| 디자이너 | spec.md | `design.md` + 화면별 스펙 + (이미지) | 각 화면이 어떤 REQ-id를 커버하는지 매핑표 |
| 퍼블리셔 | design.md | HTML/CSS 컴포넌트 | 디자인 토큰 일치 검사 + 반응형 스냅샷 |
| 개발자 | spec + 마크업 | 동작 코드 + 테스트 | 테스트 통과 + REQ-id 커버리지 |
| QA | 전부 | `qa-report.md` | REQ-id별 pass/fail + 재현 절차 |

**웹앱으로 좁힌 결과 — DoD가 기계 검증 가능해진다**
```
task.dod = [
  { "kind":"cmd",      "run":"npm test",  "expect":"exit 0" },
  { "kind":"cmd",      "run":"npm run build", "expect":"exit 0" },
  { "kind":"req_cover","ids":["REQ-1","REQ-3"] },     // 산출물이 해당 id를 참조하는지
  { "kind":"browser",  "flow":"signup", "expect":"성공 토스트 노출" }  // gstack /browse 재사용
]
```
Lead는 `task.result`를 받으면 **DoD를 직접 실행**해서 수락 여부를 판정한다.
에이전트의 자기보고를 믿지 않는다 — 이게 "만드는 AI와 평가하는 AI 분리" 원칙의 적용이다.

**핵심 장치**: 모든 산출물이 `REQ-id`를 참조합니다. PM이 "기획대로 안 됐다"를 말할 때
감이 아니라 **커버되지 않은 REQ-id**를 근거로 `change_request`를 보냅니다.

**M9 갱신 — `cmd` DoD 실행 규칙** (`crates/crew-lead/src/cmd_exec.rs`, `dod_exec::judge/3`):
Lead는 `task.result`를 받으면 위 예시의 `kind:"cmd"` 체크를 셸을 거치지 않는 argv 직접
실행으로 검증한다.
- **허용목록**: `cargo test`/`cargo build`/`cargo clippy`/`npm test`/`npm run`/`pnpm test`/
  `pnpm run`/`yarn test`/`yarn run` 9개 프리픽스만 허용(`CmdPolicy::default_allowlist`).
  프리픽스에 없으면 `Refused`.
- **위치별 허용목록** (`vet()`, r2 F3로 경화): 프리픽스 위치 토큰은 설정된 명령과의
  **리터럴 동등 비교**(별도 문자집합 검사 없음), 프리픽스 **뒤**의 모든 토큰은
  `is_bare_trailing_token` — 첫 글자가 영숫자이고 나머지는 `[A-Za-z0-9._-]`만, `..`
  부분문자열 금지. 메타문자 블록리스트가 아니라 양성 규칙이라는 점은 유지되지만, 위치마다
  다른 규칙이다. 귀결: 플래그(`-`로 시작)·절대경로(`/`로 시작)·상대경로(`.`로 시작)·
  `=`/`:`/`@`/`+`를 포함한 토큰은 트레일링 위치에서 **전부 거부**된다 — `npm run build`의
  `build`처럼 맨 식별자만 트레일링으로 허용된다. `tokio::process::Command`로 argv를 그대로
  넘기며 셸을 전혀 거치지 않는다. 위치별 허용목록은 **argv 수준의 방어**이며 패키지
  스크립트 같은 간접층은 검사하지 않는다(§5 함정 29).
- `cargo test --workspace`처럼 정당해 보이는 명령도 **거부된다**(`--workspace`가 트레일링
  플래그라서) — 이 거부는 조용한 통과가 아니라 `failed_cmds`에 남는 **보이는 실패**다. DoD를
  쓸 때 이 제약을 감안해야 한다.
- **`expect`는 `"exit <N>"` 형식만 인식**한다(`parse_expect`). 그 외 문자열(예: 위 예시의
  `browser` 체크처럼 자연어)은 체크가 아예 실행되지 않고 `skipped`로만 기록된다.
- **타임아웃 기본 120초**(`CmdPolicy::default_allowlist().timeout`). M10부터 자식은 spawn
  시 `process_group(0)`으로 자기 그룹의 리더가 되고(`cmd_exec.rs`), 타임아웃과
  `ProcessGroupGuard`의 `Drop` **양쪽**에서 `libc::killpg(pgid, SIGKILL)`로 **그룹 전체**를
  죽인다. 그룹 kill 후 `child.wait()`로 좀비를 수확한다(§5 함정 26 — **해소: M10
  t-pgroup**). 잔여 한계: 손자가 스스로 `setsid()`/`setpgid()`로 새 세션/그룹을 만들면
  `killpg`의 사정거리 밖으로 나간다(`cargo`/`npm`/`pnpm`/`yarn`은 그러지 않으므로 허용목록
  범위 안에서는 영향 없음). 근거: `docs/SPIKE-M10.md` §2 A/B 표(FIX ON: exit `0`·잔존
  프로세스 **0개** / FIX OFF: exit `101`·잔존 **1개**, 기록된 손자는 타임아웃 후에도
  `kill(pid,0)`이 생존을 반환).
- **실행 cwd** (M11 갱신, `crew-run`의 `role_cli_cwd(project_root, data_dir, role)`):
  `project_root: Some(root)`이면 해당 태스크 역할의 CLI 세션 cwd와 Cmd DoD 실행 cwd가
  **둘 다 `root`** — Lead 자신의 cwd가 아니다. `project_root: None`(기본)이면 M10까지의
  동작 그대로 스크래치 `<data_dir>/cli-cwd/<role>`.
- `kind:"browser"`는 **M10에서도 아직 미실행**이다 — 항상 `skipped`로만 기록된다(스코프
  아웃).
- 실행 결과 중 하나라도 `Refused`/`TimedOut`/`SpawnFailed`이거나 exit code가 `expect`와
  다르면 `DodVerdict.failed_cmds`에 쌓이고 `passed=false`가 되며, `AcceptanceLoop::decide`가
  이를 그대로 `Rework.violations`에 포함한다.
- **M10부터 노브 기반 방출이 생겼다** — `LeadPlanner::plan_dag_with(spec, roles,
  &PlanOptions { dev_cmd_checks })`가 **Developer 역할 태스크에만** `ReqCover` 뒤로
  `DodCheck::Cmd`를 이어붙인다(`plan.rs`; `roles`에 Developer가 없으면 조용히 무방출).
  `plan_dag_for`/`plan_dag`는 `PlanOptions::default()`로 위임하는 래퍼라 **기존 출력
  무변경**이다. 호출부 경로는 `RunConfig.dev_cmd_checks` → `crew-run/src/controller.rs`의
  `plan_dag_with` 호출(HANDOFF §3 M10). **기본값은 빈 벡터 = 방출 없음** — 이유는 아래.
- **기본값이 OFF인 이유** (M11 갱신 — A/B 실측으로 대체, `docs/SPIKE-M11.md` §2):
  `project_root: None`일 때의 실행 cwd 모양 `<data_dir>/cli-cwd/<role>`의 어떤 상위에도
  프로젝트 매니페스트(`Cargo.toml`)가 없다. 그 모양의 디렉토리에서 `cargo test`를 돌리면
  `` could not find `Cargo.toml` in `.../cli-cwd/developer` or any parent directory ``로
  exit `101`이 나고(A), `expect "exit 0"`과 불일치해 `DodVerdict.failed_cmds`에 쌓여
  리워크 루프로 간다. `project_root: Some(<레포 루트>)`로 실행 cwd를 레포 루트로 바꾸면(B)
  같은 `cargo test`가 exit `0`으로 끝난다(실측: `test result:` 합산 394 passed / 0 failed /
  9 ignored, `docs/SPIKE-M11.md` §2). **`dev_cmd_checks` 노브를 켜려면 `project_root` 지정이
  선행 조건이다** — M10까지는 이 조건이 성립하지 않아 기본 OFF였고, M11이 `project_root`를
  도입해 조건을 채웠다. 단, 이것이 함정 29를 닫지는 않는다(아래 신뢰 경계 문단, HANDOFF §5
  함정 29).
- **기본 상수**: `crew_run::default_dev_cmd_checks_rust()`/`_node()`가 위치별 허용목록
  (§5 함정 27)을 통과하는 문자열(`cargo test` / `npm test`·`npm run build`)을 제공한다.
  이는 **기본값이 아니라 호출부가 골라 쓰는 상수**다 — `RunConfig.dev_cmd_checks`의 실제
  기본값은 여전히 빈 벡터.

**신뢰 경계 (M11 신설, contracts-m11.md §I6)**: `cmd` DoD는 설계상 **에이전트 산출물의 코드
실행**이다 — Developer 에이전트가 실행 트리에 쓴 파일(빌드 스크립트, `package.json` 등)을
Lead가 그대로 실행해 판정한다. 이 실행을 봉쇄하는 것은 argv 위치별 허용목록(§5 함정 27이
막는 층)이 **아니라**, 사람이 지정한 `project_root` 트리다. `project_root`에는 **봉쇄
목록이 없다** — 검증은 "절대경로인가, 존재하는 디렉토리인가"(`RunError::ProjectRootInvalid`,
`RunController::start` 선행 검증)뿐이고, 어떤 경로가 지정 가능한지에 대한 상한이 없다.
즉 `$HOME`도 `/`도 `project_root`로 지정할 수 있고, 지정하면 **그 트리 전체**에서 에이전트가
쓴 코드가 Lead에 의해 실행된다. 위치별 허용목록(argv 경계)의 역할은 "허용된 명령 프리픽스
뒤에 플래그·경로를 못 끼워 넣게 한다"는 것으로 한정되며, 어떤 트리에서 그 명령이 도는지는
전혀 판단하지 않는다 — 그 판단은 전적으로 `project_root`를 지정하는 사람의 몫이다. 이
노브를 실제 활성화하려면(`RunConfig.project_root`를 `Some`으로) 반드시 함정 29(`HANDOFF.md`
§5 함정 29) —
`npm|yarn|pnpm run`류 간접 실행이 위치별 허용목록을 무력화한다는 사실 — 를 감안해야
한다. **함정 29는 이 마일스톤으로 닫히지 않는다.**

### 4.3 Lead 에이전트의 실제 구현
- Lead도 LLM 세션이지만, **출력은 반드시 구조화 JSON** (툴 스키마 강제).
  자유 텍스트로 계획을 뱉게 하면 파싱이 무너집니다.
- Lead는 코드를 만지지 않습니다. 오직 계획·라우팅·수락 판정·머지.
- Lead의 컨텍스트에는 원본 대화 전체가 아니라 **스펙 + DAG 상태 + 스프린트 요약**만.


### 4.4 완전 자율 모드의 안전장치 (승인 게이트를 없앤 대가)

승인 게이트가 없으면 **잘못된 계획이 조용히 3스프린트를 태울 수 있다.** 그래서 개입 대신
비차단(non-blocking) 장치 넷을 둔다.

| 장치 | 동작 |
|---|---|
| **계획 미리보기 (1회)** | 런 시작 시 Lead의 스펙+DAG를 UI에 띄우고 N초 카운트다운 후 자동 시작. 사용자가 안 보고 있으면 그대로 진행되고, 보고 있으면 그 순간 고칠 수 있다. |
| **예산 상한** | 런당 토큰/시간/스프린트 수 상한. 초과 시 자동 일시정지 + 알림. |
| **즉시 중단** | 언제든 정지 → 진행 중 태스크 graceful cancel, 상태 저장, 재개 가능. |
| **자동 에스컬레이션** | ①라운드 예산 초과 ②DoD 2연속 실패 ③워크트리 머지 충돌 ④하네스 전멸 ⑤스펙 모호(Lead 판단) → `human.gate` |

**에스컬레이션은 방해가 아니라 알림이다.** 승인함에 쌓이고, 사용자가 없으면 해당 태스크만
차단 상태로 두고 **나머지 DAG는 계속 진행**한다.

---

## 5. 컨텍스트 공유 전략 (가장 어려운 부분)

> 외부 근거 대조: [§13.4](#134-5-컨텍스트-공유-전략과의-대조), [docs/RESEARCH.md](RESEARCH.md) §2 참고.

전체 스레드를 모두에게 주면 3번째 스프린트에서 터집니다. 3계층으로 나눕니다.

```
L1 공유 사실 (모든 에이전트, 항상)   : spec.md + 결정 로그 + 아티팩트 인덱스 + 현재 스프린트 목표
                                     목표 크기: ≤ 4k 토큰. 초과 시 Lead가 압축.
L2 태스크 브리프 (해당 에이전트만)   : 내 태스크 + 의존 산출물 발췌 + DoD + 관련 대화 발췌
L3 개인 세션 (에이전트 내부)         : 해당 CLI 세션의 자체 히스토리. 외부 비공개.
```

- **이벤트 원장(append-only)**: 모든 메시지·툴콜·산출물 커밋을 SQLite에 저장.
  UI와 재생(replay), 사후 감사, 컨텍스트 재조립의 단일 출처.
- **스프린트 압축**: 스프린트 종료 시 Lead가 `sprint-N-summary.md` 생성 →
  각 에이전트 세션을 **종료하고 새 세션 + 요약으로 재시작**.
  (`--fork-session`/`--resume` 활용, 프리앰블 비용은 스프린트당 1회로 고정)
- **핸드오프 팩**: 사용량 소진/오류 시 `HandoffPack{ role, spec_ref, done, in_flight, decisions, open_questions }`
  를 만들어 **다른 하네스**에 그대로 주입. 영상에서 말한 "프롬프트로 정리해줘" 수작업의 자동화.

---

## 6. 파일 충돌 — 병렬 에이전트의 진짜 지뢰

- 에이전트마다 **git worktree + 전용 브랜치**(`crew/sprint-3/dev`). 같은 파일 동시 수정 원천 차단.
- Lead가 스프린트 종료 시 순차 머지, 충돌 시 해당 에이전트에게 `change_request`.
- 워크스페이스 루트에 `.crew/` (DB·로그·임시파일). **`/tmp` 사용 금지** — 보안 정책.
- 파일 쓰기는 워크트리 경계 밖으로 못 나가게 러너 프로세스에서 경로 검증.

---

## 7. UI 요구사항 — "한 눈에 보이게"

| 화면 | 내용 |
|---|---|
| **커맨드 바** | 요청 한 줄 입력 → Lead 기동 |
| **스프린트 보드** | 칸반(대기/진행/검토/완료/차단). 카드=태스크, 담당 에이전트 아바타 |
| **에이전트 레일** | 에이전트별 카드: 상태(대기/작업 N초/응답대기), 현재 태스크, 하네스 배지(클로드/코덱스…), 토큰·사용량 게이지 |
| **라이브 스레드** | 슬랙 형태. `task.ack`는 👀 뱃지, `change_request`는 색상 강조, 첨부 아티팩트 인라인 |
| **DAG 뷰** | 의존성 그래프. 크리티컬 패스 하이라이트, 차단 노드 빨강 (M6 구현: `features/dag`, `@xyflow/react`) |
| **타임라인** | 가로 시간축 스윔레인 — 누가 언제 뭘 했는지, 유휴 구간이 어디인지 (M6 구현: `features/timeline`) |
| **승인함** | `human.gate` 대기 목록. 클릭 한 번으로 승인/반려+사유 (M7 구현: `features/inbox`) |
| **아티팩트/디프** | 산출물 미리보기 + 커밋 디프 + REQ-id 커버리지 매트릭스 (M7 구현: `features/artifacts`) |

- 모든 뷰는 **이벤트 원장 스트림 구독**으로 갱신 (폴링 금지).
- 전역 검색: 메시지·산출물·결정 로그 (SQLite FTS5). (M7 구현: `features/search`)

---

## 8. 저장소 스키마 초안 (SQLite)

```sql
runs(id, goal, created_at, status)
sprints(id, run_id, idx, goal, status, summary_path)
tasks(id, sprint_id, role, title, brief, dod_json, deps_json, status,
      assignee, started_at, finished_at, rounds_used)
messages(id, run_id, sprint_id, thread, from_id, to_json, kind,
         in_reply_to, corr, body_json, ts)          -- append-only
artifacts(id, task_id, path, kind, req_ids_json, sha256, ts)
agents(id, role, harness, model, session_id, instructions, status)
usage(agent_id, ts, tokens_in, tokens_out, est_cost)
decisions(id, run_id, text, rationale, made_by, ts)  -- L1 컨텍스트 소스
```
`messages`는 수정·삭제 금지(감사 가능성). 해시체인은 3단계 옵션.

---

## 9. 엣지 케이스 체크리스트 (품질 100% 원칙)

- [ ] CLI가 성공을 가장한 실패 (`is_error` 판정) — **최우선**
- [ ] **같은 구독으로 다중 세션 동시 실행 → 레이트리밋/한도 조기 소진** (하네스별 세마포어)
- [ ] CLI 프로세스 크래시 / stdout 파이프 깨짐 → 러너 재시작 + 세션 resume
- [ ] 사용량 한도 도달 → handoff → 대체 하네스, 없으면 human.gate
- [ ] change_request 무한 루프 / A↔B 순환
- [ ] 태스크 타임아웃 (에이전트가 조용히 멈춤)
- [ ] DAG에 사이클 생성 (Lead가 잘못 계획) → 생성 시점에 위상정렬 검증
- [ ] 아티팩트 누락인데 `task.result` 성공 보고 → 계약 검증 후 수락
- [ ] git 머지 충돌 / 워크트리 밖 파일 쓰기 시도
- [ ] 에이전트가 다른 에이전트를 사칭 (러너 토큰으로 from 검증)
- [ ] 사용자가 중간에 중단 → 진행 중 태스크 graceful cancel + 상태 저장
- [ ] 앱 재시작 후 진행 중이던 런 복원
- [ ] 빈 입력/한 단어 요청 → Lead가 명확화 질문 1회

---

## 10. 기술 스택 결정

| 영역 | 선택 | 근거 |
|---|---|---|
| 앱 셸 | **Tauri 2** | Rust 백엔드 + 웹 UI. DAG/타임라인/스레드 같은 복잡한 뷰는 웹이 압도적으로 빠름. 번들 작음 |
| 비동기 | tokio | 프로세스·소켓·타이머 전부 |
| WS | axum + tokio-tungstenite | 서버(코어)와 클라이언트(러너) 한 스택 |
| 직렬화 | serde + JSON | CLI들이 JSON, 디버깅 용이 |
| DB | rusqlite (+FTS5) | 단일 파일, 이벤트 원장에 적합 |
| git | git2-rs 또는 `git` CLI 호출 | worktree는 CLI가 안정적 |
| 상태 | 이벤트 소싱 + 투영 | replay·감사·UI 갱신이 공짜 |
| 로그 | tracing | 스팬으로 태스크 추적 |

**대안 검토**: egui/iced 순수 Rust UI → 그래프/스레드 UI 구현 비용이 커서 비추천.
**전제 조건**: 이 맥북에 `cargo` 미설치 → `rustup` 설치 필요.

---

## 11. 로드맵 (각 단계마다 검증 가능한 성공 기준)

**M1 — 하네스 스파이크 (가장 먼저, 가장 중요)**
- claude-code 상시 세션 어댑터 1개. 이벤트 정규화, `is_error` 판정, resume.
- ✅ 성공 기준: 한 프로세스에 3연속 턴을 보내고 각 응답을 구조화 이벤트로 수신,
  강제 실패 케이스에서 `Failed` 로 정확히 분류. (테스트 3종: 정상/에러/타임아웃)

**M2 — 버스 + 2에이전트 왕복** ✅ 완료 (2026-08-27, 실측: `docs/SPIKE-M2.md`)
- WS 버스, ack/타임아웃/재시도, PM↔Designer `change_request` 루프 1회 완주.
- ✅ 스펙 위반을 심은 산출물에 대해 change_request가 발생하고 재작업 후 수락될 것.

**M3 — Lead 오케스트레이션**
- 스펙화 → DAG → 1스프린트 실행 → 수락. 5역할 전부.
- ✅ "간단한 랜딩 페이지" 요청 하나가 사람 개입 0회로 QA 통과까지 도달.

**M4 — UI**
- 스프린트 보드 + 라이브 스레드 + 에이전트 레일. (DAG/타임라인은 그다음)
- ✅ 실행 중인 런을 앱만 보고 완전히 이해 가능.

**M5 — 스프린트 압축 + 핸드오프 + 멀티 하네스**
- 로스터 프리셋 UI + 하네스 자동 탐지 + 동시성 세마포어.
- ✅ 3스프린트 연속 실행에서 에이전트 컨텍스트가 한계 미만으로 유지,
  하네스 강제 교체 후에도 작업 연속성 유지.

**3단계(후순위)**: ed25519 서명, 원격 릴레이, 사람 참여, 모바일.

---

## 12. 미해결 — 결정 필요

1. ~~첫 타깃 도메인~~ **결정됨: 웹앱 개발로 좁힌다.**
   → DoD를 `테스트 통과 + 빌드 성공 + 스냅샷 일치`로 **기계 검증**한다. QA 에이전트가
   실제 fail을 낼 수 있으므로 change_request 루프가 진짜로 돌아간다.
2. ~~하네스 범위~~ **결정됨(2026-08-27)**: 하네스는 슬롯 설정값. 클로드 단독으로 5인팀 구성이
   기본 시나리오이고, 다른 CLI/로컬 모델은 같은 어댑터 인터페이스로 나중에 꽂는다.
   → 남은 판단은 **MVP에 어댑터를 몇 개 실제로 구현할지**뿐 (claude-code 1개 + 스텁 권장).
3. ~~디자이너 산출물~~ **결정됨: 텍스트 디자인 스펙까지.**
   `design.md` = 디자인 토큰(색/타이포/간격) + 화면별 레이아웃 + 컴포넌트 명세 + REQ-id 매핑표.
   이미지 생성 하네스는 붙이지 않는다. 퍼블리셔가 토큰 일치 여부를 기계적으로 대조할 수 있다.
4. ~~사람 개입 지점~~ **결정됨: 완전 자율 + 예외만 에스컬레이션.**
   스프린트 경계 승인 게이트 없음. 대신 §4.4의 자율 안전장치로 리스크를 막는다.

### 남은 미결
5. 앱/프로젝트 이름 (`agent-crew` 가칭)

   **추천(2026-08-27, 리서치 기반)**: `crewpilot` 또는 `crewdeck`.
   근거 — `agent-crew`는 GitHub Topics에 CrewAI 기반 프로젝트가 이미 다수 태깅돼 있고
   `aibtcdev/ai-agent-crew` 저장소도 존재해 CrewAI 생태계와 혼동될 위험이 있다
   [docs/RESEARCH.md §6][R17]. `crewpilot`/`crewdeck`은 GitHub·crates.io·Homebrew
   검색 범위 내 미발견으로 상대적으로 깨끗하다 [docs/RESEARCH.md §6].
   **대안**: `swarmdeck`(마찬가지로 검색 범위 내 미발견). **트레이드오프**: 검색으로 못
   찾음은 "없다"는 뜻이 아니라 "이번 조사 범위에서 못 찾았다"는 뜻이므로, 최종 이름
   확정 전 실제 등록(GitHub org, crates.io reserve, Homebrew tap) 시점에 재확인 필요.
   `agentmux`·`teamforge`·`rolecraft`는 조사 결과 각각 개념 충돌(거의 동일한 tmux 기반
   멀티 CLI 오케스트레이터가 이미 2개 존재)·상용 제품명 충돌·동명이인(다른 도메인) 문제가
   확인되어 추천에서 제외했다 [docs/RESEARCH.md §6]. 이것은 추천이며 결정이 아니다 —
   최종 선택은 사용자 몫이다.

6. 프론트엔드 프레임워크 (Tauri 안에서 React vs Svelte vs 순수 TS)

   **추천(2026-08-27, 리서치 기반)**: **React + Vite**.
   근거 — Tauri 2 공식 보일러플레이트 생태계에서 React+Vite 조합이 "가장 흔한 조합"으로
   소개되어 예제·트러블슈팅 자료가 가장 풍부할 가능성이 높고 [docs/RESEARCH.md §7][R22],
   DAG 뷰(§7 UI 요구)에 필요한 `xyflow`가 React Flow/Svelte Flow를 동일 팀·동일 API
   철학으로 유지보수해 두 프레임워크 모두 DAG 시각화는 검증된 선택지다 [docs/RESEARCH.md
   §7][R27]. 1인 개발 생산성 기준으로는 React 생태계 자료량이 더 많을 가능성이 있다는
   정황은 있으나 이를 정량 비교한 출처는 찾지 못했다 [docs/RESEARCH.md §7].
   **대안**: Svelte — 번들 크기가 더 작다는 서술이 반복적으로 확인되고
   [docs/RESEARCH.md §7][R22][R23][R26], Tauri 앱 자체가 이미 Electron 대비 5~10MB로
   작아(§10) 번들 크기 이점의 체감 효과가 제한적이라는 게 이번 추천에서 React를 우선한
   이유다. **트레이드오프**: 팀이 나중에 커지거나 번들 크기가 실제 병목으로 확인되면
   Svelte로 전환하는 옵션은 열려 있다 — 이것은 추천이며 결정이 아니다.

---

## 13. 리서치 근거와 보강 (2026-08-27)

> t-docs 태스크 산출물. 브레이브서치로 조사한 외부 근거를 §2~§5 설계와 대조한다.
> 전문·출처 표는 [docs/RESEARCH.md](RESEARCH.md)에 있다 — 아래는 그 요약과 대조 결과만 발췌.
> 기존 §0~§12 본문은 이 섹션과 위쪽 각 섹션 도입부의 링크 한 줄 외에는 수정하지 않았다.

### 13.1 §2 구독 에이전트를 붙이는 법과의 대조

| 일치하는 외부 근거 | 상충·보강 제안 |
|---|---|
| §2.2의 "`claude -p`는 실패해도 success를 뱉는다" 위험은 Claude Code 공식 에러 레퍼런스의 사례(헤드리스 모드에서 컨텍스트 초과 에러가 나도 런이 계속 진행됨)와 같은 방향이다 [R4]. | stream-json 종료 이벤트의 `is_error`/`terminal_reason` 필드는 Anthropic이 필드 단위로 완전히 공개한 스펙이 없다는 지적이 있다 [R6]. **보강 제안**: 어댑터 구현 시 필드 누락을 기본값 처리하는 방어적 파싱을 §2.3 `Harness` 트레이트 문서에 명시할 것. |
| §2.4의 "같은 구독 동시 세션 → 레이트리밋 공유" 제약은 Anthropic 공식 문서(5시간 세션 단위 집계)와 제3자 관찰(동시 worktree 세션이 주간 Opus 한도를 소진) 양쪽에서 뒷받침된다 [R7][R8][R9][R10]. | 공식 문서는 정확한 동시 세션 허용치를 공개하지 않는다 [R9]. **보강 제안**: §2.4의 "예: `claude-code: max 3 concurrent`"는 임의 상수로 못 박기보다, 런타임에 한도 응답을 관측해 세마포어 크기를 적응적으로 낮추는 로직으로 보강 검토. |

### 13.2 §3 메시지 프로토콜과의 대조

| 일치하는 외부 근거 | 상충·보강 제안 |
|---|---|
| §3.4의 "corr당 최대 왕복 3회, 순환 감지" 설계는 Anthropic 멀티에이전트 연구 시스템이 겪은 "서브에이전트 간 중복 작업·종료 판단 실패" 실패 모드와 같은 문제의식이다 [R3]. | 상충 없음. Anthropic 사례는 왕복 제한이 아니라 사전에 작업을 명확히 분할해 중복을 막는 접근(계획을 메모리에 저장)에 가깝다 [R3]. **보강 제안**: §3.4의 사후 감지(왕복 카운트) 외에, Lead가 태스크 배정 시점에 중복 검색/작업 방지를 위한 사전 체크(예: 같은 자원에 대한 진행 중 태스크 존재 여부)를 추가하는 것을 검토. |

### 13.3 §4 오케스트레이션 모델과의 대조

| 일치하는 외부 근거 | 상충·보강 제안 |
|---|---|
| §4.3 "Lead는 코드를 만지지 않고 계획·라우팅·수락 판정만" 원칙은 Anthropic 사례의 오케스트레이터-워커 분리와 동일 패턴이며, 이 패턴이 단일 에이전트 대비 90.2% 성능 우위를 보였다는 근거가 있다 [R3]. | §4.1의 "리드 에이전트가 서브에이전트를 동기적으로 실행하고 기다리는" 방식은 Anthropic도 "조율은 단순해지지만 정보 흐름 병목을 만든다"고 지적한 지점과 겹친다 [R3]. **보강 제안**: §4.1 ④ 디스패치 단계에서 의존성이 없는 태스크는 비동기·병렬로 흘려보내는 설계(현재도 "의존성 풀린 태스크 병렬 실행"이라고 되어 있어 방향은 맞음)를 §4.3에도 명시적으로 재확인. |
| §4.4의 "완전 자율 모드 + 예외만 에스컬레이션" 안전장치는 Anthropic이 겪은 "사소한 시스템 오류가 완화 장치 없이는 치명적으로 번질 수 있다"는 경고와 부합하는 방향이다 [R3]. | 상충 없음. |

### 13.4 §5 컨텍스트 공유 전략과의 대조

| 일치하는 외부 근거 | 상충·보강 제안 |
|---|---|
| §5의 3계층 컨텍스트(L1 공유 사실/L2 태스크 브리프/L3 개인 세션)와 "스프린트 압축" 설계는 Anthropic 사례의 "작업 단계를 요약해 외부 메모리에 저장, 컨텍스트 한계 도달 시 저장된 계획에서 복구" 패턴과 방향이 같다 [R3]. | §5의 "핸드오프 팩"은 Anthropic의 "서브에이전트가 결과물을 외부 시스템에 저장하고 가벼운 참조만 리드에게 돌려준다"는 아티팩트 시스템과 같은 문제의식이다 [R3]. **보강 제안**: 상충은 없으나, Anthropic 사례가 명시한 "20만 토큰 초과 시 truncate" 같은 구체적 임계값을 §5의 "L1 ≤ 4k 토큰" 기준과 나란히 두고 L2/L3에도 유사한 명시적 상한을 정하는 것을 향후 태스크에서 검토. |
| 멀티에이전트 시스템의 토큰 소비량이 채팅 대비 약 15배라는 수치는 [R3], agent-crew가 5개 에이전트를 상시 세션으로 유지하는 기본 시나리오(§2.4)의 비용 구조가 가볍지 않을 것임을 뒷받침한다. | 상충 없음 — 오히려 §5의 압축 전략이 왜 "가장 어려운 부분"으로 강조돼 있는지에 대한 외부 근거가 된다. |

### 13.5 유사 OSS 대조 및 이름/FE 추천 요약

- §12 미해결 1~4번(결정됨)을 뒤집는 근거는 조사 범위에서 발견되지 않았다.
- 유사 OSS 조사([docs/RESEARCH.md §5](RESEARCH.md)) 결과, "여러 구독 CLI를 팀으로 묶는다"는
  문제의식 자체는 이미 여러 프로젝트가 다루고 있어 agent-crew의 차별점은 DAG+DoD 자동 진행
  루프, change_request 왕복 제한, Tauri 통합 UI 조합에 있다는 것이 이번 조사로 뒷받침된다.
- §12 미해결 5(이름)·6(프론트엔드)의 리서치 기반 추천안은 §12 해당 항목 아래에 직접
  기록했다 (형식: 추천 + 근거 [R-n] + 대안 + 트레이드오프, "결정" 아님).
