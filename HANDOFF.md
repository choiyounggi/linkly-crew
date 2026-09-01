# 세션 인계 — linkly-crew

**한 줄**: 구독 중인 AI CLI들을 역할별 팀원으로 묶어, 요청 한 줄을 팀장 에이전트가
스프린트로 쪼개고 에이전트끼리 협업시켜 완주시키는 macOS 앱 (Rust/Tauri). 현재 **M1~M11 완료·실측 검증**
(멀티 스프린트 + 압축 + 핸드오프/스왑 + 하네스 레지스트리/세마포어 + 로스터 UI + LLM 스펙화 +
Cmd DoD 플래너 방출 + 타임아웃 프로세스 그룹 kill) — 다음은 §4 잔여·3단계 후보.

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

**M7 확정 사항** (2026-08-30, Phase 0 사용자 결정 — `.orchestration/contracts-m7.md`):
- 범위: 가변 로스터 인원 + §7 잔여 뷰 3종(승인함 실개입·아티팩트/디프+REQ 매트릭스·FTS5
  전역 검색).
- 수동 검증: GUI 1클릭 + 가변 로스터 real-CLI 스팟체크(함정 19).
- 승인함은 실개입: 반려=즉시 Blocked 캐스케이드, 승인=재할당.
- FTS5: 기존 테이블 무변경, 별도 가상 테이블을 동일 트랜잭션에서 갱신.
- 스코프 아웃(E0): 역할당 다중 에이전트, 커스텀 역할(Role enum 5종 밖), 적응형 세마포어,
  opencode 실 어댑터, 아티팩트 백엔드 버전 저장(디프는 클라이언트 계산), 멀티 런 검색
  (검색은 현재 런 원장만), 스폰 세션 훅 차단, Cmd/Browser DoD 실행. `--ignored` real-CLI
  테스트는 전 워커 금지(코디네이터 수동 전용).

**M8 확정 사항** (2026-08-31, Phase 0 사용자 결정 — `.orchestration/contracts-m8.md`):
- 범위: 적응형 세마포어(§13.1 보강 제안) + PiHarness(opencode 실 어댑터를 대체 — DESIGN
  §2.3 2순위 갱신, 스텁 존치).
- PiHarness는 최소 가용(스폰/턴/종결 판정/방어 파싱, claude 어댑터 패리티 아님).
- 검증: 결정론 fake + real-CLI `#[ignore]` 스팟체크(코디네이터 수동, 함정 19).
- 스코프 아웃(F0): opencode 실 구현(스텁 그대로 존치·삭제 금지), pi 확장(extension)
  작성, pi 프로바이더/모델 등록 자동화(유저의 기존 pi 설정을 그대로 사용), pi 버전 자동
  감지/핀 강제, 로스터 GUI 변경(registry가 Real로 노출하면 기존 GUI가 자동 표시 — 검증만),
  실 레이트리밋 유도 실험(결정론 fake로만 검증), 어댑터 스트리밍 스티어링(steer)·
  compact·fork. `--ignored` real-CLI 테스트 실행은 전 워커 금지(추가만 — 코디네이터
  수동 전용).

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

**검증됨 — M6** (2026-08-29, 실측: 통합 브랜치 ea5fd4a):
- 스코프: mid-sprint 즉시 스왑(t-ctrl 제어 채널 + t-swapnow 3단계) + DAG 뷰(t-dag) +
  타임라인 스윔레인(t-timeline) 3건 (가변 로스터 인원·적응형 세마포어·opencode 실 어댑터는
  D0로 스코프 아웃).
- 스왑 의미론 보정(M5 대비): 스왑은 **즉시 실효**(로스터·봉투·`RosterChanged` 즉시 +
  라이브 워커 snapshot→shutdown→lazy respawn, ack 대기 `SWAP_ACK_TIMEOUT_MS=120_000`);
  3단계 실패 시 `SwapIncomplete` 반환하되 1~2단계(로스터·봉투)는 유지되어 다음 스프린트
  경계에서 실효(M5 폴백 유지) — `crates/crew-run/tests/m5_swap.rs`.
  `AgentControl`/`run_with_control`/`on_control`: `crates/crew-agent/src/control.rs·
  runner.rs·role.rs·harness_behavior.rs`.
- 결정론: 워크스페이스 31 스위트 green(rc=0), src-tauri 10/10, vitest 119/119, vite build
  green. 리워크 0라운드.
- 통합 이음새 1건: `App.test.tsx`가 t-ui-shell 스텁 문구를 직접 단언 → t-dag/t-timeline
  머지로 문구 교체되며 깨짐 → 루트 클래스 계약 단언으로 수정(commit `de8c07c`).
- 신규: `features/dag`(`buildDagView`·크리티컬 패스·`@xyflow/react` 렌더),
  `features/timeline`(`buildTimeline`·레인·마커), `LiveHandles.controls`.
- real-CLI 스팟체크 **PASS** — 2스프린트 `RealCli` 런에서 mid-sprint designer 스왑
  ack `Ok` + 개입 0회 완주, 178.97s (`crates/crew-run/tests/m5_swap.rs` `#[ignore]`
  테스트, 코디네이터 실행). 이 검증이 M5 잠재 결함 2건을 발견·수정함: ① `RealCli`
  워커 cli-cwd 미생성 → spawn ENOENT 행 (`e761ed3`) ② `HarnessPool` 퍼밋을 러너
  수명 내내 보유 → claude-code=2 리밋에서 3번째 워커부터 영구 대기 — 퍼밋을 턴 단위로
  보정 (`with_pool`, `b25ff04` + `656d763`, 계약 D2d).
- M6 커밋: `9eb368d`(t-ctrl) `de8c07c`(이음새 수정) `312bfd6`(t-swapnow) `a56338f`(t-dag)
  `e5fa4fa`(t-timeline).
- M6 계약 정본: `archive-20260829-m6a/contracts-m6.md`(예정) — 현재는
  `.orchestration/contracts-m6.md`. D1(제어 채널) D2(스왑 3단계) D7(문서).

**검증됨 — M7** (2026-08-30, 실측):
- 머지 완료 12/12 태스크: t-dagvar(E1 plan_dag_for), t-gate-lead(E2/E3 human.response+lead
  개입), t-fts(E6 FTS5), t-ui-shell2(E8 셸 확장), t-rosterrun(E4 crew_agents/
  validate_roster/가변 스폰), t-inbox(E9 승인함), t-artifacts(E10 아티팩트/디프+REQ
  매트릭스), t-search(E11 전역 검색), t-roster-ui(E12 가변 슬롯), t-gate-run(E5 human
  프록시/resolve_gate), t-bridge3(E7 Tauri 커맨드 2종/미니 프리셋/set_roster 검증),
  t-docs2(E14 본 태스크).
- 결정론: `cargo test --workspace` 통합 브랜치 green(전 크레이트, 실패 0); vitest
  (apps/crew-app) 22파일 164 테스트 green.
- real-CLI 스팟체크(함정 19 근거, 코디네이터 수동): `m7_roster::
  real_cli_three_person_team_completes_one_sprint` — 3인팀(lead+developer+qa) `RealCli`
  1스프린트 완주, **78.87s**, 통과(2026-08-30). 상세: `docs/SPIKE-M7.md`.
- 간헐 플레이크 관측: `cargo test --workspace`가 콜드 빌드 직후 첫 실행에서만 2회 실패
  목격 — 1차 `m7_roster::three_person_team_run_completes_...`(4회 런 중 1회), 2차 미상
  3-테스트 바이너리 1건(2 passed; 1 failed; 0.10s). 직후 재실행은 각각 3+·6연속 clean,
  격리 실행 8/8 clean, 실패 메시지 미포착 — 원인 미상(병렬 부하/콜드 스타트 타이밍
  의심). §5 함정 20 참고.
- 오케스트레이션 운영 관측: m7a 재진입 시 4개 태스크(t-artifacts/t-inbox/t-rosterrun/
  t-search)가 `impl_done` 보고 후 커밋 전 워커 사망 — 구현이 워크트리 dirty로만
  존재했고 코디네이터가 스냅샷 커밋으로 회수. §5 함정 21 참고.
- GUI에서 scripted=false 실 런 1클릭: 미실행(사람 몫 — §4에 유지).
- M7 계약 정본: `.orchestration/contracts-m7.md` (teardown 시
  `archive-20260830-m7a/contracts-m7.md`로 아카이브 예정 — M6 표기 관례와 동일).

**검증됨 — M8** (2026-08-31, 실측):
- 머지 완료 4/4 태스크(코드 3 + 본 문서): t-pool-adapt(F1 적응형 세마포어 —
  `report_rate_limit`에 한도 −1·하한 1·쿨다운 멱등 추가, lazy 회복 +1/300s, 기존
  백오프 verbatim 유지, Semaphore→Mutex+Notify 교체), t-pi-harness(F3 PiHarness
  최소 가용 — pi v0.75.5 RPC 모드, `AgentCfg.model` 추가, `Session.stdin`
  `Arc<Mutex>` 보정, registry `KNOWN` 7종·pi=Real, 픽스처 6종 단위 테스트),
  t-pool-wire(F2 — `looks_like_rate_limit` 술어 7신호 + `RoleHarnessBehavior`
  Failed 경로 배선).
- 결정론: `cargo test --workspace` 통합 브랜치 green(실패 0). crew-harness 신규
  테스트: pool 5종(축소/하한/멱등/회복/catch-up), pi 단위 6종 + registry 경계 1.
  crew-agent 신규: 술어 5 + 통합 2.
- pi RPC 스파이크(코디네이터, 2026-08-31, 원시 캡처
  `~/.linkly-crew/pi-spike/rpc2.jsonl`): ① stdin EOF 시 즉시 셧다운(턴 완료 전
  절단) ② 정상 턴 = `message_end`(stopReason "stop") → `turn_end` →
  `agent_end`(willRetry:false); 문서상 `agent_settled`는 90초 내 미발화 — 종결
  판정 사용 금지 ③ `extension_ui_request` 1턴 89건(무응답형 스팸), 다이얼로그형
  (select/confirm/input/editor)은 응답 없으면 스톨 위험. 상세: `docs/SPIKE-M8.md`.
- real-CLI 스팟체크(함정 19, 코디네이터 수동): `crew-harness tests/real_pi.rs
  one_turn_round_trip` — 실제 pi 프로세스 1턴 라운드트립, **6.70s**, 통과
  (2026-08-31).
- M8 계약 정본: `.orchestration/contracts-m8.md` (teardown 시
  `archive-20260831-m8a/contracts-m8.md`로 아카이브 예정 — M7 표기 관례와 동일).

**검증됨 — M9** (2026-08-31, 실측: 통합 브랜치 703bf18):
- 머지 완료 3/3 코드 태스크 + 본 문서: t-hook(스폰 세션 유저 전역 훅 차단),
  t-cmd(`DodCheck::Cmd` 허용목록 argv 실행기 + 판정), t-wire(실 `task.result` 경로 배선).
- t-hook: `ClaudeCodeHarness::with_setting_sources`(기본 `Some("project,local")`)를
  `spawn`·`spawn_resumed` 양쪽에 적용(`crates/crew-harness/src/claude.rs`). 실측
  (`docs/SPIKE-M9.md`, claude 2.1.236): `hook_started` 이벤트 9→0, `apiKeySource:"none"`
  (구독 OAuth 유지) A/B 동일, 부수 효과로 프리앰블 24,667→9,998 토큰(-14,669, 함정 2 완화).
  프로덕션 형상(`--session-id` + stream-json stdin) 조합에서도 훅 0건 확인.
- t-cmd: `crates/crew-lead/src/cmd_exec.rs` 신규 — `CmdPolicy::default_allowlist()`(프리픽스
  9종, 타임아웃 120s), 위치별 허용목록(셸 미경유, `tokio::process::Command` argv 직접
  실행 — 프리픽스 토큰은 리터럴 동등 비교, 프리픽스 뒤 모든 토큰은 `is_bare_trailing_token`
  으로 영숫자 시작 + `[A-Za-z0-9._-]`만 + `..` 금지, 그 결과 플래그·절대/상대경로는 트레일링
  위치에서 전부 거부), `parse_expect`(`"exit <N>"`만 인식). `dod_exec::judge`가 `judge/3`으로
  확장되어 `DodVerdict.failed_cmds` 추가. `accept.rs`도 함께 수정됨(계약 밖 추가) —
  `AcceptanceLoop::decide`가 `failed_cmds`를 `Rework.violations`에 체이닝(리뷰 t-cmd-r1 F1
  회귀 방지 — Cmd 체크 단독 실패도 진단 없이 빈 violations로 돌아가지 않는다).
- **r2 보안 수정** (`ff9636a`, Phase 5 통합 리뷰 발견): 초기 구현은 프리픽스만 검사했다 —
  `cargo test --manifest-path=<레포 밖>`이 `vet()`을 통과해 cwd 샌드박스를 벗어나 임의
  `build.rs`를 실행하면서도 exit 0으로 DoD를 통과시켰다. 위 위치별 허용목록(트레일링 토큰의
  `is_bare_trailing_token` 규칙)이 이 플래그 인젝션 경로를 닫은 것 — §5 함정 27 참고.
- t-wire: `LeadBehavior::with_cmd_exec`/`with_cmd_policy`가 `crew-run`
  `controller.rs`의 `spawn_sprint`에서 무조건 배선됨(`role_cli_cwd(data_dir, role)` 클로저로
  실행 cwd 주입). `resolve_task_result`를 `handle_task_result`에서 분리(동기 유지) —
  비동기 Cmd 실행은 `on_envelope` 안에서 `handle_task_result` 호출 전에 일어난다.
  `ObservingLead::on_envelope`는 변경 없이 그대로 위임.
- **Cmd DoD는 실 런에서는 아직 휴면이다.** 실행기·판정·배선은 완성·테스트됐지만
  `LeadPlanner::plan_dag_for`(`crates/crew-lead/src/plan.rs:93`)는 `DodCheck::ReqCover`만
  방출하고, `plan_llm.rs`엔 `DodCheck` 참조가 아예 없으며(스펙 문서만 생성),
  `RunConfig`에도 DAG 직접 주입 필드가 없다 — scripted/LLM 두 경로 모두
  `controller.rs:493`에서 같은 `plan_dag_for`를 호출한다. 즉 **현재 어떤 코드 경로도
  `DodCheck::Cmd`를 만들지 않는다** — M10에서 해소(아래 "검증됨 — M10" 참고).
- 결정론: `cargo test --workspace` rc=0, **357 passed, 0 failed, 9 ignored**(직접 실행,
  `--ignored` 미실행 — 함정 19; r2 보안 수정으로 `cmd_exec.rs` 테스트가 7개 늘어
  350→357).

**검증됨 — M10** (2026-08-31, 실측: `crew-m10-integration`, t-pgroup `6108698` + t-cmdplan
`2947ad1`, 머지 `cf342f5`; 상세: `docs/SPIKE-M10.md`):
- t-pgroup: `cmd_exec.rs`가 spawn 시 `#[cfg(unix)] cmd.process_group(0)`로 자식을 새
  그룹의 리더로 만들고, 타임아웃·`ProcessGroupGuard::Drop` 양쪽에서
  `libc::killpg(pgid, SIGKILL)`로 그룹 전체를 죽인 뒤 `child.wait()`로 수확한다(§5 함정
  26 해소). 루트 `Cargo.toml`의 `tokio`를 `"1"` → `"1.40"`으로 핀하고 `libc = "0.2"`를
  추가했다.
- t-cmdplan: `LeadPlanner::plan_dag_with(spec, roles, &PlanOptions { dev_cmd_checks })`가
  Developer 태스크에만 `ReqCover` 뒤로 `DodCheck::Cmd`를 이어붙인다. `plan_dag_for`/
  `plan_dag`는 `PlanOptions::default()`로 위임하는 래퍼로 기존 출력 무변경.
  `RunConfig.dev_cmd_checks`(기본 빈 벡터) → `controller.rs`의 `plan_dag_with` 호출로
  배선. `crew_run::default_dev_cmd_checks_rust()`/`_node()`가 위치별 허용목록을 통과하는
  상수를 제공(§5 함정 27).
- 실측(코디네이터, `docs/SPIKE-M10.md` §2 A/B — 함정 26 재현): FIX ON
  (`process_group(0)`+`killpg`): 테스트 exit `0`, 잔여 `sleep 300` **0개**. FIX OFF(M9
  의미론): 테스트 exit `101`, **기록된 손자 생존**(`kill(pid,0)`이 0 반환), 잔여
  `sleep 300` **1개**(포그라운드 손자).
- 핀 근거(`docs/SPIKE-M10.md` §1): tokio `CHANGELOG.md` — `process_group` 추가
  `1.22.0`(2022-11-17), stabilize `1.40.0`(2024-08-30).
- 스위트(`docs/SPIKE-M10.md` §4): t-pgroup 워크트리 `cargo test --workspace` rc=0,
  **360 passed / 0 failed**(베이스라인 357 + 신규 3). t-cmdplan 워크트리 rc=0,
  **371 passed / 0 failed**(베이스라인 357 + 신규 14).

**검증됨 — M11** (2026-08-31, 실측: `crew-m11-integration` 베이스 `49509dc`, t-vet
`de68636` + t-cwd `35e6736` 머지; 상세: `docs/SPIKE-M11.md`):
- t-vet: `crates/crew-lead/src/cmd_exec.rs`에 `CmdPolicy::vet_run(&str) -> Result<(), String>`
  공개 파사드 신설(변경 파일 1개, cmd_exec.rs 단독) + 인라인 테스트 7개 추가.
- t-cwd: `crates/crew-run/src/config.rs`에 `RunConfig.project_root: Option<PathBuf>` 신설
  (기본 `None` = M10까지 동작 그대로) + `RunController::start` 선행 검증(절대경로·존재
  디렉토리, 아니면 `RunError::ProjectRootInvalid`) + `crates/crew-run/src/controller.rs`의
  `role_cli_cwd(project_root, data_dir, role)`가 에이전트 CLI 세션 cwd와 Cmd DoD 실행 cwd
  양쪽에 배선됨 — `project_root: Some(root)`이면 둘 다 `root`를 그대로 반환(스크래치
  `cli-cwd/` 서브디렉토리 미생성), `None`이면 M10까지의 `<data_dir>/cli-cwd/<role>` 그대로.
  `m11_project_root.rs` 신규 9테스트, src-tauri `core.rs` 호출부 + `RunConfig` 생성부
  6곳 갱신.
- 실측(코디네이터, `docs/SPIKE-M11.md` §2 A/B): `project_root: None`(A) — 실행 cwd에
  매니페스트 없어 `cargo test` exit `101`. `project_root: Some(<레포 루트>)`(B) — exit `0`,
  `test result:` 합산 394 passed / 0 failed / 9 ignored. 이 A/B가 §4 잔여 4번이 실제로
  걷힌 벽임을 증명한다.
- t-docs(본 문서 태스크): `crates/crew-run/src/config.rs` 테스트의 허용목록 이중 정의
  제거(`CmdPolicy::vet_run` 직접 호출로 교체) + `crates/crew-lead/src/plan.rs`의 항진
  테스트 `plan_dag_with_default_options_equals_plan_dag_for` 삭제. `cargo test --workspace`
  rc=0, **393 passed / 0 failed / 9 ignored**(394에서 항진 테스트 1개 삭제로 감소 —
  `docs/SPIKE-M11.md` §3), `cargo check --all-targets --manifest-path
  apps/crew-app/src-tauri/Cargo.toml` rc=0.
- **M11 결정 — 해소 아님**: 함정 29(`npm|yarn|pnpm run` 간접 실행이 허용목록을 무력화)는
  이 마일스톤으로 닫히지 않았다. `--ignore-scripts`가 이 경로를 막는지 실측했으나
  막지 못했다(`docs/SPIKE-M11.md` §1, 4/4 rc=0). §5 함정 29 참고.
- M11 계약 정본: `archive-20260831-m11a/contracts-m11.md` §I5·§I6·§I7·§I8.

**미검증**:
- GUI에서 scripted=false 클릭 실행(네이티브 창 — 사람 1클릭 필요; 실 CLI 경로 자체는 위
  E2E 2건으로 증명됨) — Gate 2에서 확인 예정.
- Browser DoD 실제 실행(M3부터 skip 기록만, M9 G0로 스코프 아웃 — 착수 전).
- Cmd DoD의 실 런 발화(§3 M9 참고 — 메커니즘은 완성됐으나 플래너 미방출로 휴면).

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

## 4. 다음 스텝 — M10 완료 후 잔여

M1~M11 완료·실측 검증됨(§3). 다음 후보:

**M10 완료 후 잔여 (작은 것부터)**:
1. GUI에서 scripted=false 실 런 1클릭 확인 (사람 1분 — 네이티브 창이라 자동화 불가;
   현재 코디네이터 수동 진행 중).
2. Browser DoD 실제 실행 (M3부터 skip 기록만, M9 G0로 스코프 아웃 — 착수 전).
3. auto-memory 스폰 세션 주입 차단(함정 8 잔여 한계 ① — `--setting-sources`로는 못 막는다).
4. **Cmd DoD 실행 cwd를 실제 프로젝트 루트로** — 이것이 없으면 `dev_cmd_checks` 노브를
   켤 수 없다. 지금 켜면 실행 cwd(`role_cli_cwd`)에 매니페스트가 없어 `cargo test`가
   exit `101`("could not find `Cargo.toml`")로 끝나고 `failed_cmds`에 쌓인다
   (`docs/SPIKE-M10.md` §3). 이 변경은 함정 29(`npm|yarn|pnpm run` 간접 실행이 허용목록을
   무력화)를 무장시키므로, cwd 변경과 함께 `npm|yarn|pnpm run` 허용 여부를 반드시
   재결정해야 한다.
   **완료 — 단, 함정 29는 닫히지 않음(§5)** (M11, t-cwd `35e6736`): `RunConfig.project_root`
   신설 + `role_cli_cwd(project_root, data_dir, role)`가 CLI 세션 cwd/Cmd DoD 실행 cwd
   양쪽에 배선. `project_root` 지정 시 exit `101`(A) → exit `0`(B) 실측(`docs/SPIKE-M11.md`
   §2). "`npm|yarn|pnpm run` 허용 여부를 반드시 재결정해야 한다"는 이 문장의 요구는
   **재결정하지 않고 그대로 남겨뒀다** — 재결정은 M11 스코프 밖(DESIGN §4.2 신뢰 경계
   문단 참고).
5. **허용목록 규칙의 이중 정의 제거** — `crates/crew-run/src/config.rs`의 테스트 모듈이
   `ALLOWED_PREFIXES`와 `is_bare_trailing_token`을 `cmd_exec.rs`에서 복제하고 있다.
   두 태스크가 머지된 지금은 `CmdPolicy`에 `vet_run(&str) -> Result<(), String>`를
   공개해 프로덕션 판정기를 직접 호출하도록 바꿀 수 있다.
   **완료** (M11, t-docs): `crates/crew-run/src/config.rs` 테스트 모듈이 `CmdPolicy::
   default_allowlist().vet_run(&c.run)`을 직접 호출하도록 교체, `ALLOWED_PREFIXES`·
   `is_bare_trailing_token`·`assert_satisfies_positional_allowlist` 3개 전부 삭제
   (grep 0건).
6. **`plan_dag_with_default_options_equals_plan_dag_for`는 항진 명제** —
   `plan_dag_for`가 `plan_dag_with`에 위임하므로 자기 자신과 비교한다. 진짜 회귀
   가드는 `plan.rs`의 기존 5역할 DAG 테스트(`dod == [ReqCover]` 단언)다. 정리 대상.
   **완료** (M11, t-docs): 항진 테스트 삭제(grep 0건), 회귀 가드
   `plan_dag_builds_a_validated_linear_five_role_chain`의 `dod == [ReqCover]` 단언은
   그대로 통과(`cargo test --workspace` rc=0).

opencode 실 어댑터는 PiHarness로 대체됨(스텁 존치, DESIGN §2.3) — M8 F0로 스코프
아웃 항목에서 제거. 적응형 세마포어는 M8 F1로 완료.

**3단계(DESIGN §11 후순위)**: ed25519 서명, 원격 릴레이, 사람 참여, 모바일.

**재사용 계약**: M3(archive-20260827-m3a/contracts-m3.md) + M4(archive-20260828-m4a/
contracts-m4.md) + M5(archive-20260829-m5a/contracts-m5.md) + M6(archive-20260829-m6a/
contracts-m6.md) + M7(archive-20260830-m7a/contracts-m7.md) + M8(archive-20260831-m8a/
contracts-m8.md) + M9(archive-20260831-m9a/contracts-m9.md) +
**M10(archive-20260831-m10a/contracts-m10.md)** — RunEvent 3종, TaskStateDto
"blocked", HandoffPack/Roster, HarnessRegistry/HarnessPool, LlmLeadPlanner,
`RunHandle::swap_harness`(즉시 실효 + 경계 폴백), `AgentControl`/`on_control`,
`features/dag`/`features/timeline`, Tauri 커맨드 5종, 로스터 패널/동적 배지/이니셜 맵,
`plan_dag_for`(가변 역할 DAG), `agent:human`/`GateDecision`/`RunHandle::resolve_gate`,
crew-ledger FTS5 `search_messages`/`RunHandle::search_messages`, `features/inbox`/
`features/artifacts`/`features/search`, 가변 로스터 슬롯 UI(추가/삭제), Tauri
`resolve_gate`/`search_messages` 커맨드 + 미니 2인팀 프리셋, `HarnessPool` 적응형
세마포어(`report_rate_limit` 한도 축소/회복, `RATE_RECOVERY_SECS=300`),
`crew_harness::pi::PiHarness`(`HarnessId("pi")`), `AgentCfg.model`,
`Session.stdin`(`Arc<Mutex<ChildStdin>>`), `crew_agent::harness_behavior::
looks_like_rate_limit`, `ClaudeCodeHarness::with_setting_sources`(기본
`Some("project,local")`), `crew_lead::cmd_exec::{CmdPolicy,CmdOutcome,parse_expect,
execute_cmd_checks}`, `dod_exec::judge/3`·`DodVerdict::failed_cmds`,
`LeadBehavior::with_cmd_exec`/`with_cmd_policy`, `crew_lead::plan::{CmdCheck,
PlanOptions}`, `LeadPlanner::plan_dag_with`, `RunConfig.dev_cmd_checks`,
`crew_run::{default_dev_cmd_checks_rust, default_dev_cmd_checks_node}`, `cmd_exec`의
프로세스 그룹 종료(`process_group(0)`/`ProcessGroupGuard`/`killpg`, 타임아웃·드롭
양쪽) + **M11(`archive-20260831-m11a/contracts-m11.md` §I5·§I6·§I7·§I8)** — 신규 공개 API 2건:
`RunConfig.project_root`(`Option<PathBuf>`, 기본 `None`, `RunError::ProjectRootInvalid`로
선행 검증), `crew_lead::cmd_exec::CmdPolicy::vet_run(&str) -> Result<(), String>`.
재발명 금지.

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
8. **cwd 격리는 유저 전역 훅을 못 막는다 — M9에서 해결** (M3 real-CLI E2E 실측,
   2026-08-27; 해소: M9 t-hook, 2026-08-31): 스폰된 claude 세션 안에서 유저 전역 Stop 훅
   (learning-nudge 등)이 발화해 유효한 계약 JSON 뒤에 프로즈 턴("Learning review: …")이
   붙었고, 드레인 텍스트 전체 파싱만 하던 `extract_json`이 실패 → Blocked → 에스컬레이션
   → 후속 태스크 영구 대기로 E2E 타임아웃. **해결**: `ClaudeCodeHarness::with_setting_sources`
   기본값 `Some("project,local")`를 `spawn`·`spawn_resumed` 양쪽에 적용해 스폰 세션이
   유저 전역 설정 소스를 아예 로드하지 않게 했다(`crates/crew-harness/src/claude.rs`).
   실측(`docs/SPIKE-M9.md`, claude 2.1.236): `hook_started` 이벤트 9→0,
   `apiKeySource:"none"`(구독 OAuth 유지)는 적용 전/후 동일 — 부수 효과로 프리앰블
   24,667→9,998 토큰(-14,669, 함정 2 완화)도 확인. **잔여 한계 3건**: ① auto-memory는
   여전히 주입된다(`memory_paths.auto`가 적용 전/후 동일 경로 — 설정 소스와 무관해
   `--setting-sources`로 막히지 않는다, SPIKE-M9 §4) ② `extract_json`의 문자열 인지
   균형 중괄호 스캔 폴백은 **제거 금지** — 훅 차단이 막는 건 훅발 프로즈 오염뿐이고,
   모델 자체가 JSON 앞뒤에 설명을 붙이는 경우엔 여전히 이 폴백이 유일한 방어선이다
   ③ 프로젝트 훅은 계속 로드된다 — `project,local`을 남긴 선택의 귀결이며, 스폰 대상
   워크스페이스가 자체 훅을 두면 그대로 발화하는 것은 의도된 동작이다.
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
17. **제어 채널은 턴 사이에만 처리** (M6 결정, `AgentControl`/`on_control`): 하네스가
    현재 턴을 스트리밍하는 도중엔 스왑 명령을 끼워 넣지 않고 턴 경계까지 대기 후 처리한다
    (ack 대기 상한 `SWAP_ACK_TIMEOUT_MS=120_000`의 근거 — 진행 중인 턴이 길면 그만큼
    ack가 늦어질 수 있음을 전제).
18. **셸 태스크 테스트가 스텁 문구를 단언하면 후속 교체에서 깨진다** (M6 통합 이음새 1건,
    `de8c07c`): `App.test.tsx`가 t-ui-shell이 심은 플레이스홀더 문구를 직접 문자열
    단언했다가, t-dag/t-timeline이 실제 뷰로 교체하며 실패 — 크로스 태스크 스텁을
    단언할 땐 문구가 아니라 **루트 클래스/구조 계약**(안정적으로 유지되는 것)을
    단언할 것.
19. **`RealCli` 멀티워커 경로는 결정론 테스트로는 안 잡히는 실환경 결함이 있다** (M6
    real-CLI 스팟체크 실측): cwd 부재로 인한 spawn ENOENT, 세마포어 퍼밋 스코프(러너
    수명 전체 보유 시 리밋 초과 워커 영구 대기) 둘 다 결정론(`Scripted`) 스위트는
    통과했지만 실 CLI 2스프린트 런에서만 드러났다 — 새 `RealCli` 경로를 만들면 반드시
    실 CLI 스팟체크 1회를 코디네이터 수동으로 돌릴 것.
20. **`cargo test --workspace`가 콜드 빌드 직후 첫 실행에서만 간헐 플레이크를 낸다**
    (M7 실측, 코디네이터 스팟체크): 2회 실패 목격 — 1차 `m7_roster::
    three_person_team_run_completes_...`(4회 런 중 1회), 2차 미상 3-테스트 바이너리
    1건(2 passed; 1 failed; 0.10s). 직후 재실행은 각각 3+·6연속 clean, 격리 실행
    8/8 clean, 실패 메시지도 미포착 — 원인 미상(병렬 부하/콜드 스타트 타이밍 의심).
    재현 불가·근본원인 미확정이므로 결론 내리지 말 것 — 콜드 빌드 직후 실패를 보면
    바로 결함으로 단정하지 말고 한 번 더 돌려 재현 여부부터 확인할 것.
    3차(m9a Phase 5 통합 테스트, 2026-08-31, 코디네이터 실측): `crew-bus`의
    `tests/integration.rs:171` `test_redelivery_after_no_receipt_succeeds`가 콜드 빌드 직후
    `cargo test --workspace` 첫 실행에서만 실패. **이번에 처음으로 실패 메시지가 포착됐다** —
    `assertion left == right failed / left: Error { code: "delivery_failed", message:
    "msg_01M1B169TRTQQ8FT89AC13NB97" } / right: Receipt { id: "msg_..." }`. 즉 무수신 후
    재배달이 `Receipt` 대신 `delivery_failed`를 돌려줬다 — 재배달 타이밍이 병렬 부하에서
    밀리는 쪽을 시사한다(확정 아님). 직후 재현 시도: 격리 실행 3/3 clean, `-p crew-bus`
    스위트 3/3 clean, `cargo test --workspace` 2연속 clean(357 passed). M9는 `crates/crew-bus`를
    **한 줄도 건드리지 않았다**(`git diff 8e331e7..crew-m9-integration -- crates/crew-bus`가 빈
    출력) — M9 회귀가 아니다.
21. **워커가 `impl_done` 보고 후 커밋 전에 죽으면 구현이 워크트리 dirty로만 존재한다**
    (M7 오케스트레이션 운영 실측, m7a 재진입): 4개 태스크(t-artifacts/t-inbox/
    t-rosterrun/t-search)가 이 상태로 발견됐고, 코디네이터가 스냅샷 커밋으로 회수했다
    — merge-prep 프롬프트(§4, 커밋 지시) 전달 전 워커 생존을 확인할 것.
22. **`pi --mode rpc`는 stdin EOF 시 턴 완료 전이라도 즉시 셧다운한다** (M8 RPC
    스파이크 실측, 코디네이터, 2026-08-31, `docs/SPIKE-M8.md`): 어댑터는 세션 수명
    동안 stdin 파이프를 열어둬야 한다 — 이 실측이 `Session.stdin`을
    `ChildStdin` → `Arc<tokio::sync::Mutex<ChildStdin>>`로 바꾼 근거(F3).
23. **pi의 `agent_settled`는 종결 판정에 쓸 수 없다** (M8 RPC 스파이크 실측): 문서상의
    `agent_settled` 이벤트가 90초 내 미발화하는 것을 실측했다. 정상 턴 순서는
    `message_end`(stopReason "stop") → `turn_end` → `agent_end`(`willRetry:false`) —
    `agent_end`(willRetry:false)를 턴 종결 백스톱으로 삼을 것, `agent_settled`를
    기다리지 말 것.
24. **pi의 `extension_ui_request` 다이얼로그형 메서드는 응답 없으면 스톨한다** (M8 RPC
    스파이크 실측): 1턴에 89건(setWidget/setStatus/setTitle 등 무응답형 스팸) 발생,
    그중 다이얼로그형(select/confirm/input/editor)은 응답이 없으면 진행이 막힌다 —
    PiHarness는 즉시 `{"type":"extension_ui_response","id":<id>,"cancelled":true}`로
    자동 응답해 스톨을 방지한다.
25. **`test-floor.sh`의 파일 분류기에 Rust 항목이 없다** (m9a 오케스트레이션 운영 실측,
    t-cmd): `src/*.rs` 안의 인라인 `#[cfg(test)] mod tests`를 인식하지 못해
    `no-tests`(exit 3)를 낸다 — 실제로는 `cmd_exec.rs` 한 파일에만 테스트 10개·assert
    16개가 있었다. 이 도구의 `no-tests` 판정을 리워크 사유로 삼지 말고 `floor=unknown`으로
    취급할 것.
26. **Cmd DoD 타임아웃은 직계 자식만 죽인다** (`crates/crew-lead/src/cmd_exec.rs`
    `execute_cmd_checks` — `kill_on_drop`/`child.kill()` 둘 다 직접 spawn한 자식
    프로세스 핸들에만 작용): `cargo test`/`npm test`처럼 자기 자식을 낳는 런처가
    타임아웃되면 손자 프로세스가 남을 수 있고, 이들이 `target/` 락을 쥐면 같은 런의 다음
    `cargo build` 체크도 연쇄 타임아웃될 수 있다. M9 의도적 스코프 아웃(리뷰 t-cmd-r1
    N1에서 "stands"로 수용) — 프로세스 그룹(setsid + 그룹 kill) 도입은 후속 과제.
    **해소: M10 t-pgroup**: spawn 시 `#[cfg(unix)] cmd.process_group(0)`로 자식을 새
    그룹의 리더로 만들고, 타임아웃과 `ProcessGroupGuard`의 `Drop` 양쪽에서
    `libc::killpg(pgid, SIGKILL)`로 그룹 전체를 죽인 뒤 `child.wait()`로 좀비를
    수확한다. 실측(`docs/SPIKE-M10.md` §2, A/B): FIX ON은 잔여 `sleep 300` **0개**,
    FIX OFF(M9 의미론)는 손자가 `kill(pid,0)`에 생존을 반환하며 잔여 **1개**로 남는다.
    **잔여 한계 2건**: ① 손자가 스스로 `setsid()`/`setpgid()`로 새 세션/그룹을 만들면
    `killpg`의 사정거리 밖으로 나간다(`cargo`/`npm`/`pnpm`/`yarn`은 그러지 않으므로
    허용목록 범위 안에서는 영향 없음) ② non-unix 빌드는 `process_group`/`killpg`가
    없어 기존 `kill_on_drop` 경로(직계 자식만 종료) 그대로다.
27. **명령 허용목록에서 프리픽스만 검사하면 플래그 인젝션으로 뚫린다** (Phase 5 통합 리뷰
    실측, m9a — `crates/crew-lead/src/cmd_exec.rs` `vet()`, t-cmd r2 `ff9636a`로 수정):
    초기 구현은 `["cargo","test"]` 같은 프리픽스만 리터럴 비교하고 그 뒤 토큰은 검사하지
    않았다 — `cargo test --manifest-path=<레포 밖>`이 그대로 통과해 cwd 샌드박스를 벗어난
    임의 `build.rs`를 실행하면서도 exit 0으로 DoD를 통과시켰다. 수정: 프리픽스 뒤 모든
    토큰에 `is_bare_trailing_token`(영숫자 시작 + `[A-Za-z0-9._-]`만 + `..` 금지)을 적용해
    플래그(`-`)·절대/상대경로(`/`·`.`)를 전부 거부. **새 프리픽스를 추가할 때마다 그 명령의
    플래그가 cwd/manifest/config를 바꿀 수 있는지 확인할 것.**
28. **주입된 `DodCheck::Cmd`의 파싱 가능한 `expect`는 재귀 cargo 위험이 있다** (M10
    실측, `docs/SPIKE-M10.md` §3.1): `crew-run` 테스트에서 파싱되는 `expect`를 가진
    `DodCheck::Cmd`를 주입하면 실제 서브프로세스가 뜬다 — 실행 cwd가
    `crates/crew-run` 아래에 중첩돼 cargo가 상위로 올라가 crew-run 자신의
    `Cargo.toml`을 찾으므로 중첩 `cargo test`가 되고 바깥 런과 `target/` 락을 다툰다.
    통합 테스트는 파싱 불가 sentinel `expect`를 써서 실행을 0으로 만든다
    (`crates/crew-run/tests/m10_cmd_dod.rs` 모듈 doc).
29. **`npm|yarn|pnpm run <script>`의 간접 실행이 허용목록을 무력화한다** (M10 Phase 5
    통합 리뷰 발, 코디네이터 재현): 재현(2026-08-31)
    ```
    $ mkdir -p ~/.linkly-crew/m10-attack-probe && cd $_
    $ cat > package.json
      { "name":"probe", "scripts": { "build": "echo ARBITRARY_CODE_EXECUTED > ./pwned.txt; exit 0" } }
    $ npm run build ; echo $?
    0
    $ cat pwned.txt
    ARBITRARY_CODE_EXECUTED
    ```
    `vet()`가 보는 것: 토큰 `[npm, run, build]` → 프리픽스 `[npm, run]`이 리터럴 일치,
    트레일링 `build`는 맨 식별자 → **허용**. 셸도, 플래그도, 경로도 없다. 함정 27이
    막으려던 것을 전부 지켰는데도 임의 코드가 실행됐고, **exit 0이라 DoD가 통과한다**
    — 함정 27의 플래그 인젝션은 최소한 실패로라도 드러났지만 이쪽은 **조용히 통과**한다.
    이것이 실제 위협인 이유: `crates/crew-run/src/controller.rs`에서 에이전트의 CLI
    세션 cwd(298행 `cwd: role_cli_cwd(data_dir, role)`)와 Cmd DoD의 실행
    cwd(335행 `with_cmd_exec(... role_cli_cwd(&cmd_cwd_base, role))`)가 **같은
    디렉토리**다 — Developer 에이전트가 자기 작업 디렉토리에 쓰는 `package.json`을
    그대로 DoD 실행기가 읽는다. 스크립트 이름은 허용목록이 볼 수 없는 간접층이다.
    **오늘은 발화하지 않는다** — `dev_cmd_checks`의 기본값이 빈 벡터이고 GUI가
    `Vec::new()`를 하드코딩한다. 그러나 §4 잔여 4번("Cmd DoD 실행 cwd를 실제 프로젝트
    루트로")이 바로 이것을 무장시키는 변경이다. **완화 방향(선택지, 미확정)**: 실행
    cwd를 에이전트 쓰기 영역과 분리하거나, `npm/yarn/pnpm run`을 허용목록에서 빼거나,
    스크립트 본문까지 검증하는 것 중 하나가 §4 잔여 4번과 함께 결정돼야 한다.

    **M11 결정 — 해소 아님.** §4 잔여 4번은 M11에서 완료됐다(`project_root` 도입, 위
    "완료 — 단, 함정 29는 닫히지 않음" 참고) — 즉 위 문단이 예고한 무장은 실제로
    일어났다. 그러나 이 함정 자체는 **닫히지 않았다.** 위에서 미확정으로 남긴 세 완화
    방향은 각각 다음 근거로 죽었다(실측: `docs/SPIKE-M11.md` §1):
    - **(a) `npm run` 제거**: 허용목록에서 `npm run`/`pnpm run`/`yarn run`을 빼도
      `npm test`가 동일한 간접층이다 — `package.json`의 `scripts.test`도 에이전트가
      쓰고 `npm test`가 그대로 실행한다. 프리픽스 하나를 빼는 것으로는 막히지 않는다.
    - **(b) 스크립트명 화이트리스트**: 스크립트 **이름**(`build`/`test` 등)을
      화이트리스트해도 그 이름이 가리키는 **본문**은 에이전트가 쓴 `package.json`이
      정의한다 — 이름이 무해해 보여도 본문은 임의 셸 명령일 수 있다. 이름 검증은
      본문을 전혀 보지 못한다.
    - **(c) 스크립트 본문 검증**: 채택하지 않았다. 셸 명령 문자열을 안전하게
      정적 검증하는 것은 위치별 허용목록(함정 27)이 이미 겪은 문제(플래그·경로
      인젝션)를 셸 문법 전체로 확장하는 것이라 이 마일스톤 스코프 밖.
    - `--ignore-scripts`도 막지 못한다(§1 참고) — `npm run <script>`/`npm test`로
      **명시 호출된** 스크립트는 이 플래그가 막는 라이프사이클 훅과 다른 경로다.
      실측: `npm run build --ignore-scripts` rc=0 (`pwned-build.txt` 생성),
      `npm test --ignore-scripts` rc=0 (`pwned-test.txt` 생성) — 4/4 rc=0.

    남은 상태: `project_root`를 지정하고 `dev_cmd_checks`에 `npm run`/`npm test`류를
    포함시키면, 함정 29가 조용히 통과한다. 봉쇄는 argv 허용목록이 아니라 `project_root`를
    지정하는 사람의 판단뿐이다(DESIGN §4.2 신뢰 경계 문단).
30. **`project_root: Some`이면 로스터 전원의 CLI 세션 cwd가 동일하다 — 락·역할별 브랜치·
    충돌 감지가 전부 없다** (Phase 5 통합 리뷰 발, 코디네이터 재현, 2026-08-31):
    1. `role_cli_cwd`의 `Some(root)` 분기(`controller.rs:98-101`)는 역할과 무관하게
       `root.to_path_buf()`를 그대로 반환한다 — 역할별 하위 디렉토리를 만들지 않는다.
    2. `spawn_sprint`(`controller.rs:277`)는 `crew_agents(roster)`로 얻은 로스터 전원을
       매 스프린트 동시에 `tokio::spawn`한다 — 이번 스프린트에 태스크가 있는 역할만이
       아니다. `RealCli` 모드에서는 5개 CLI 세션이 동시에 산다. `SprintSlicer::slice`
       (`plan.rs:232`)는 위상 순서를 `max_per_sprint` 단위로 `chunks()`할 뿐이라, 한
       스프린트에 서로 다른 역할의 태스크가 함께 들어간다.
    3. 귀결: 락·역할별 브랜치·충돌 감지가 **전부 없으므로** 마지막 writer가 조용히
       이긴다. 더해서 Lead의 `Cmd DoD`(`cmd_exec::execute_cmd_checks`)도 같은 cwd에서
       도므로, 아직 쓰는 중인 형제 세션의 파일을 읽어 **코드 정확성과 무관한 DoD
       판정**이 나올 수 있다.
    4. **DESIGN §6의 "에이전트마다 git worktree + 전용 브랜치"는 미구현이다**
       (`grep -rn "worktree" crates/ apps/crew-app/src-tauri/src/`의 히트는 전부
       dev-loop 워크트리를 가리키는 테스트 주석 — 제품 코드의 역할별 git
       worktree/브랜치는 어디에도 구현돼 있지 않다). M10까지는 역할별 scratch
       디렉토리(`<data_dir>/cli-cwd/<role>`)가 우연히 그 역할을 대신하고 있었고,
       M11이 `project_root: Some`으로 그것을 없앴다.
    5. **오늘은 발화하지 않는다** — `project_root` 기본값이 `None`이고 GUI에 피커가
       없다. §4-1(GUI 1클릭)이나 `project_root` 피커를 붙이는 것이 이것을 무장시킨다.
    6. 검증은 런 시작 1회뿐이며, 실행 중 그 트리가 사라지거나 교체되는 것은 재검증하지
       않는다(에이전트 세션 자신이 그 트리에 쓰기 권한을 갖는다는 점과 함께 읽을 것).
