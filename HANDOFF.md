# 세션 인계 — linkly-crew

**한 줄**: 구독 중인 AI CLI들을 역할별 팀원으로 묶어, 요청 한 줄을 팀장 에이전트가
스프린트로 쪼개고 에이전트끼리 협업시켜 완주시키는 macOS 앱 (Rust/Tauri). 현재 **M1~M5 완료·실측 검증**
(멀티 스프린트 + 압축 + 핸드오프/스왑 + 하네스 레지스트리/세마포어 + 로스터 UI + LLM 스펙화) — 다음은 M5 잔여·3단계 후보.

**이름 확정(2026-08-28, 사용자 결정)**: 프로젝트명 **linkly-crew** (구 가칭 agent-crew).
프론트엔드 **React 19 + Vite** (DESIGN §12.6 추천안 채택).

---

## 1. 읽을 파일

| 순서 | 파일 | 비고 |
|---|---|---|
| 1 | `HANDOFF.md` (이 파일) | 상태·결정·다음 스텝 |
| 2 | `docs/DESIGN.md` (398줄) | 설계 전문. **이것만 읽으면 됨** |

`docs/DESIGN.md` 안에서 우선순위 높은 섹션:
- **§2** 구독 에이전트 붙이는 법 — 프로젝트의 심장. 여기가 틀리면 전부 무너짐
- **§2.4** 로스터 — 역할/하네스 분리. 클로드 하나로 5인팀이 기본 시나리오
- **§3** 에이전트 간 메시지 프로토콜 (ack / change_request 루프)
- **§4.2** 산출물 계약 + 기계 검증 가능한 DoD
- **§5** 컨텍스트 3계층 압축
- **§9** 엣지 케이스 체크리스트
- **§11** 로드맵 (M1~M5, 각 단계 성공 기준 포함)

자동 로드되는 관련 메모리: `claude-cli-headless-success-signal.md`
(= `claude -p`는 실패해도 `subtype:"success"`. `is_error`/`terminal_reason`로 판정. 호출당 프리앰블 40~52k)

---

## 2. 이번 세션에서 확정된 것

| 항목 | 결정 |
|---|---|
| 타깃 도메인 | **웹앱 개발로 좁힘** → DoD를 테스트/빌드/브라우저 플로우로 기계 검증 |
| 사람 개입 | **완전 자율 + 예외만 에스컬레이션** (승인 게이트 없음, §4.4 안전장치로 대체) |
| 디자이너 산출물 | **텍스트 디자인 스펙까지** (토큰/레이아웃/컴포넌트 명세). 이미지 생성 없음 |
| 하네스 | 역할과 완전 분리. 슬롯 설정값. 클로드 단독 5인팀이 기본 |
| 스택 | Tauri 2 + tokio + axum/tokio-tungstenite + rusqlite(이벤트 원장) |

**남은 미결 2건**: 프로젝트 이름(`agent-crew`는 가칭), Tauri 내부 프론트엔드(React/Svelte/순수 TS)

---

## 3. 검증 상태 (중요)

**검증됨** (2026-08-27, 이 맥북에서 `claude --help` 실측):
- `claude -p --input-format stream-json --output-format stream-json --session-id <uuid>`
  → 프로세스 상주 + 양방향 스트리밍 입력 지원. API 키 없이 구독 그대로 사용 가능.
- `-r/--resume`, `--fork-session`, `--agents <json>`, `--include-partial-messages` 존재 확인.

**검증됨 — M1** (2026-08-27, 실측: `crates/crew-harness/SPIKE.md`):
한 프로세스에 3연속 턴 + resume 1턴, 각 응답 구조화 이벤트 수신, 강제 실패 `Failed` 정확 분류.

**검증됨 — M2** (2026-08-27, 실측: `docs/SPIKE-M2.md`):
- 결정론 통합테스트: 실제 `BusServer` + `BusConn` 2개 + `ScriptedPm`/`ScriptedDesigner`
  (planted `REQ-2`) → `PmState::Accepted{rounds_used:1}` 직접 단언, `DeliveryFailed`/
  `LoopBlocked` 미발생, 워크스페이스 스위트 green (0.32s).
- real-`claude` E2E(`--ignored`): 실제 `claude` CLI + `DesignerHarnessBehavior`로 동일
  시나리오 완주 — 2턴, elapsed=9.836s, `rounds_used:1`.
- `CorrGuard`는 `ChangeRequest`/`TaskResult`/`ReviewRequest`만 가드하므로(`TaskAck`/
  `TaskAssign` 제외), 위반 1건=리워크 1라운드 시나리오는 `max_rounds=3` 예산 중 2만 사용 —
  여유 있음.

**검증됨 — M3** (2026-08-27, 실측: `crates/crew-lead/tests/m3_sprint.rs`):
- 결정론 3시나리오: ①해피 — 5역할(ScriptedCrewMember) 스프린트 완주, ChangeRequest 0·
  HumanGate 0, SQLite 원장에 Delivered 행 기록 ②리워크 — planted `REQ-2` → Lead의
  DoD 직접 판정(`dod_exec::judge`)이 uncovered 검출 → ChangeRequest 정확히 1회 → 수락
  ③에스컬레이션 — 미등록 역할 → `human.gate` 정확히 1회, 나머지 태스크는 계속 수락.
- real-`claude` E2E(`--ignored`): **"간단한 랜딩 페이지" 요청 → Lead 스펙화 → 5역할 전부
  실제 claude 세션 → DoD 판정 → 전 태스크 수락, 사람 개입 0회, 136.65s** (§11 M3 기준 충족).
- 신규 크레이트/모듈: `crew-proto`(SpecDoc/TaskSpec/TaskDag/DodCheck), `crew-ledger`
  (append-only 원장), `crew-lead`(plan: 결정론 스펙화·5역할 DAG·스프린트 슬라이싱 /
  dispatch·accept·dod_exec: 디스패치·수락/리워크 예산 2·에스컬레이션),
  `crew-agent::ScriptedCrewMember`/`RoleHarnessBehavior`, 버스 스푸핑 거부+seen 프루닝.
- Lead의 스펙화는 **M3 결정론 템플릿**(REQ-1..5 고정) — LLM 스펙화(§4.3 구조화 JSON)는
  같은 시그니처로 후속 교체 예정.

**검증됨 — M4** (2026-08-28, 실측: 통합 테스트 + 브라우저 QA 3회 전체 런):
- 데이터 파이프라인: `crew_bus::BusEvent::EnvelopeAccepted{envelope}`(수락 시 1회 브로드캐스트,
  스푸핑 거부·중복 제거 후, 재배달 미재발행) → `crew-ledger` `messages` 테이블(events와
  단일 트랜잭션, `INSERT OR IGNORE` 멱등) + `messages_since`/`messages_in_thread`.
- **`crates/crew-run`** (신규): m3_sprint.rs 조립 패턴을 승격한 런 컨트롤러 —
  `RunController::start(RunConfig{goal,mode,data_dir,max_rework}) -> RunHandle{run_id,
  subscribe->RunEvent, snapshot, join, shutdown}`. `RunMode::Scripted{planted_violations}`
  (결정론) / `RealCli`(타입만, 테스트 미실행). ObservingLead(RecordingLead 승격)가
  `TaskStateChanged` 방출. 결정론 3시나리오(해피/리워크/경계) green.
- **`apps/crew-app`** (신규, standalone — src-tauri에 자체 빈 `[workspace]`): Tauri 2 +
  React 19 + Vite + zustand. 커맨드 바 + 스프린트 보드(칸반 5컬럼) + 라이브 스레드
  (ack 👀 접기·change_request 강조·아티팩트 인라인) + 에이전트 레일(working/awaiting/idle
  파생). Tauri 브리지: `start_run/stop_run/run_snapshot` 커맨드 + `"run://event"` 펌프 +
  `TauriEventSource`(스냅샷 복원·last_seq 병합·복원 중 라이브 버퍼링). 브라우저(비 Tauri)
  에선 MockEventSource가 M3형 랜딩페이지 런 재생.
- 실측: Rust 워크스페이스 24스위트 + vitest 64/64 + src-tauri core 3케이스 green;
  브라우저 QA(aside)로 목 런 시작→리워크 뱃지→완료 5/5 전 과정 시각 확인, 콘솔 에러 0
  — §11 M4 성공 기준("앱만 보고 런 이해 가능") 충족.
- M4 계약 정본: `archive-20260828-m4a/contracts-m4.md` (EnvelopeAccepted·messages 스키마·
  RunEvent JSON·**seq 공간 규정(Message.seq=messages 테이블 / BusLifecycle.seq=events 테이블,
  비교 금지)**·Tauri 커맨드/이벤트 이름·TS 인터페이스).

**검증됨 — M5** (2026-08-29, 실측: 런 m5a, main=b1f7255):
- 결정론: 멀티 스프린트(SprintSlicer 슬라이스 순차 실행 + 경계마다 `compress::summarize_sprint`
  요약 ≤16k chars 불변식 + 세션 재시작 + `with_prior_states` 주입) 3스프린트 해피 green;
  크로스 스프린트 캐스케이드(Escalated→prior Blocked→전이, 2스프린트 건너) green;
  스왑(`RunHandle::swap_harness` — 로스터 즉시 갱신·handoff 봉투 원장 직접 기록·RosterChanged,
  실효는 다음 스프린트 경계) 6시나리오 green; 에스컬레이션 타임아웃+캐스케이드(함정 9 해소,
  timeout=0 기본 비활성) green. 워크스페이스 259 + src-tauri 10 + vitest 94/94 전부 green.
- 실 CLI(코디네이터 수동): `LlmLeadPlanner::specify` real-`claude` 17.1s 유효 SpecDoc;
  5역할 실 CLI 스프린트 E2E 193.2s 개입 0회. `cargo tauri dev` 실 GUI 기동·생존 확인.
- 브라우저 QA(aside, 목 런): 7/7 PASS 콘솔 에러 0 — 3스프린트 재생, designer 배지
  claude-code→opencode 전환, handoff 메시지, 이니셜 LD/PM/DS/PB/DV/QA 고유, 로스터
  프리셋 3종(클로드 5인팀/절약 모드/혼합 실험) 라이브 동작.
- 신규: crew-harness `HandoffSnapshot`/`Harness::snapshot`/`HarnessRegistry`/`HarnessPool`
  (FIFO+백오프, claude-code=2)/`OpencodeHarness` 스텁, crew-proto `HandoffPack`/`Roster`,
  crew-lead `compress`/`plan_llm`/`TaskState::Blocked`/on_tick, crew-run 멀티 스프린트
  컨트롤러+스왑, Tauri 커맨드 5종+`~/.linkly-crew/roster.json`, 프론트 로스터 패널·동적 배지.
- M5 계약 정본: `archive-20260829-m5a/contracts-m5.md` (RunEvent 3종 추가·TaskStateDto
  "blocked"·HandoffPack/Roster 스키마·레지스트리/풀 API·Tauri 커맨드·스왑 보정 2건).

**미검증**:
- GUI에서 scripted=false 클릭 실행(네이티브 창 — 사람 1클릭 필요; 실 CLI 경로 자체는 위
  E2E 2건으로 증명됨). mid-sprint 즉시 세션 교체(스왑 실효는 스프린트 경계 — 계약 보정).
- Cmd/Browser DoD 실제 실행(M3부터 skip 기록만), 멀티 스프린트 real-CLI 전체 런.

**환경** (2026-08-27 갱신):
- `rustup` 설치 완료, `cargo 1.98.0`(`rustc 1.98.0`) 사용 가능
- Cargo 워크스페이스 시드 완료: `crates/crew-harness`, `crates/crew-proto` (아직 스텁)
- 설치된 CLI: `claude`(/opt/homebrew/bin), `opencode`(/opt/homebrew/bin)
- 미설치: `codex`, `gemini`, `grok`, `cursor-agent`, `amp`, `qwen`
- 이 저장소는 **git init 완료** (`crew/t-docs` 등 태스크 브랜치로 오케스트레이션 진행 중)

**이번 런 진행 상황**: M5 오케스트레이션 런(11태스크, run-id m5a) 완료 — 전부 `main`에
머지됨(b1f7255). 리워크 0라운드, 플랜 갭 리플랜 2회(t-swap: SPRINT_LABEL 접근·RunError
스코프 / t-specify-llm: extract_json 재수출 스코프 — 둘 다 코디네이터 플랜 수정으로 해결).
운영 이슈 1건: 장수 tmux 서버 OAuth 만료(함정 16) → 새 소켓 서버로 전환. 런 기록은
`archive-20260829-m5a/` 참조. M3 봉투 body 계약 정본은 여전히
`archive-20260827-m3a/contracts-m3.md` (task.assign body=`{"task": TaskSpec}`,
task.result body=`{"covered_req_ids","artifacts"}` 인밴드).

---

## 4. 다음 스텝 — M5 잔여 + 3단계 후보

M1~M5 완료·실측 검증됨(§3). 다음 후보:

**M5 잔여 (작은 것부터)**:
1. GUI에서 scripted=false 실 런 1클릭 확인 (사람 1분 — 네이티브 창이라 자동화 불가).
2. **mid-sprint 즉시 스왑 실효**: 워커 핸들 레지스트리(에이전트별 세션 제어 채널) 도입 후
   스왑 시 snapshot→shutdown→respawn 즉시 수행 (현재는 다음 스프린트 경계 실효 — 계약 보정).
3. DAG 뷰/타임라인 (DESIGN §7 — M4·M5에서 의도적으로 제외).
4. 가변 로스터 인원(플래너가 5역할 DAG 고정이라 프리셋은 배치만 다름 — 계약 C0),
   적응형 세마포어(§13.1), opencode 실 어댑터.

**3단계(DESIGN §11 후순위)**: ed25519 서명, 원격 릴레이, 사람 참여, 모바일.
그 외: 스폰 세션 훅 차단(함정 8), Cmd/Browser DoD 실제 실행.

**재사용 계약**: M3(archive-20260827-m3a/contracts-m3.md) + M4(archive-20260828-m4a/
contracts-m4.md) + **M5(archive-20260829-m5a/contracts-m5.md)** — RunEvent 3종 추가,
TaskStateDto "blocked", HandoffPack/Roster, HarnessRegistry/HarnessPool, LlmLeadPlanner,
`RunHandle::swap_harness`, Tauri 커맨드 5종, 로스터 패널/동적 배지/이니셜 맵. 재발명 금지.

---

## 5. 함정 (이미 알고 시작할 것)

1. `claude -p`의 가짜 success — 실패를 완료로 올리면 오케스트레이터가 다음 스프린트로 넘어감. **최악의 실패 모드**
2. 태스크마다 프로세스 새로 띄우기 금지 — 프리앰블 40~52k가 매번 증발
3. 같은 구독 다중 세션 = 레이트리밋 공유 → 하네스별 동시성 세마포어 필수
4. 전체 스레드를 모든 에이전트에게 공유 금지 → 3스프린트째에 터짐 (§5 3계층)
5. 병렬 에이전트 동일 파일 수정 금지 → 에이전트별 git worktree
6. `/tmp` 사용 금지 (보안 정책) → `.crew/` 사용
7. crew-harness 이벤트 채널(64)은 소비자가 send와 동시에 드레인해야 함 — 순차 드레인은
   실 CLI에서 교착 (`DesignerHarnessBehavior`가 `drain_until_terminal`을 `harness.send()`
   호출 *전에* `tokio::spawn`하는 이유. M2 real-CLI E2E로 실측: `docs/SPIKE-M2.md` 참고)
8. **cwd 격리는 유저 전역 훅을 못 막는다** (M3 real-CLI E2E 실측, 2026-08-27): 스폰된
   claude 세션 안에서 유저 전역 Stop 훅(learning-nudge 등)이 발화해 유효한 계약 JSON 뒤에
   프로즈 턴("Learning review: …")이 붙었고, 드레인 텍스트 전체 파싱만 하던 `extract_json`이
   실패 → Blocked → 에스컬레이션 → 후속 태스크 영구 대기로 E2E 타임아웃. 현재 `extract_json`
   (crew-agent/harness_behavior.rs)은 문자열 인지 균형 중괄호 스캔 폴백으로 완화되어 있다 —
   이 폴백을 제거하지 말 것. 스폰 세션의 훅 완전 차단은 미해결 과제.
9. 에스컬레이션(`Escalated`)된 태스크의 스프린트 내 후속 태스크는 영구 `Pending`
   (is_done 도달 불가 가능) — M3 수용 결정. 멀티 스프린트(M5)에선 타임아웃/캐스케이드 정책 필요.
10. **열린 SQLite 원장 밑에서 디렉토리 rename 금지** (M4 결정): SQLite는 저널/WAL을
    연결 시점 경로 기준으로 만들므로, data_dir 이름을 나중에 run_id로 맞추려는 rename은
    후속 저널 생성을 깨뜨릴 수 있다. 그래서 §C6는 디렉토리명=launch-id(≠run_id)로 개정됨.
11. **RunEvent의 seq는 두 공간** (M4 계약): `Message.seq`=messages 테이블,
    `BusLifecycle.seq`=events 테이블 — 절대 비교 금지. 스냅샷 병합은 messages seq로만.
12. **일괄 생성 시나리오의 ts는 배달 시점에 stamp** (M4 리워크 1의 교훈): 빌드 시
    stamp하면 전 이벤트 동일 시각 — 단위 테스트는 통과하고 화면에서만 드러난다.
    진행성(단조 증가) 단언을 테스트에 넣을 것.
13. **에스컬레이션 캐스케이드는 타임아웃 경유만** (M5 결정): 스프린트 내 Escalated dep은
    타임아웃 전까지 후속을 Waiting으로 두고, 만료 시(또는 prior_states의 비-Accepted dep)
    전이적으로 Blocked. timeout=0(기본)이면 M3 동작 그대로. RoleBehavior 틱은 디폴트
    메서드라 **래퍼(ObservingLead 등)가 tick_interval/on_tick을 위임 안 하면 조용히
    죽는다** — 새 래퍼를 만들면 반드시 위임 + on_tick 후 diff.
14. **RosterAgent.role은 문자열** ("lead" 포함 — crew_proto::Role enum엔 Lead가 없다):
    lead 슬롯 스왑은 HandoffPack(role: Role 타입)을 만들 수 없어 봉투 생략, RosterChanged만.
15. **RunEvent 추가 시 seq 공간·ts stamp 규칙 승계**: SprintStarted/Finished/RosterChanged엔
    seq가 없고(비교 대상 아님), handoff 봉투는 원장 직접 append 경유라 Message.seq
    (messages 공간)를 정상 소비 — 버스로 보내면 "agent:lead" identity 충돌이 난다(계약 C5c 보정).
16. **오케스트레이션 운영**: 장수 tmux 서버(수일 전 기동)의 워커는 OAuth 갱신이 불가능해
    "Login expired"로 턴이 즉사할 수 있다 — 새 소켓(`TMUX_TMPDIR`)의 새 tmux 서버로
    재기동하면 해결 (m5a 런 실측, 코디네이터 셸은 정상인데 tmux 자식만 실패하는 패턴).
