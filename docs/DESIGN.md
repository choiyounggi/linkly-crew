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
| **DAG 뷰** | 의존성 그래프. 크리티컬 패스 하이라이트, 차단 노드 빨강 |
| **타임라인** | 가로 시간축 스윔레인 — 누가 언제 뭘 했는지, 유휴 구간이 어디인지 |
| **승인함** | `human.gate` 대기 목록. 클릭 한 번으로 승인/반려+사유 |
| **아티팩트/디프** | 산출물 미리보기 + 커밋 디프 + REQ-id 커버리지 매트릭스 |

- 모든 뷰는 **이벤트 원장 스트림 구독**으로 갱신 (폴링 금지).
- 전역 검색: 메시지·산출물·결정 로그 (SQLite FTS5).

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

**M2 — 버스 + 2에이전트 왕복**
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
6. 프론트엔드 프레임워크 (Tauri 안에서 React vs Svelte vs 순수 TS)
