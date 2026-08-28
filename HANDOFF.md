# 세션 인계 — agent-crew

**한 줄**: 구독 중인 AI CLI들을 역할별 팀원으로 묶어, 요청 한 줄을 팀장 에이전트가
스프린트로 쪼개고 에이전트끼리 협업시켜 완주시키는 macOS 앱 (Rust/Tauri). 현재 **M1~M3 완료·실측 검증**
(코어 백엔드 전부 동작: 하네스·버스·5역할·Lead·원장) — 다음은 **M4 UI**.

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

**미검증**:
- M4(UI): 스프린트 보드 + 라이브 스레드 + 에이전트 레일
- 멀티 스프린트 연속 실행·컨텍스트 압축(M5), Cmd/Browser DoD 실제 실행(M3는 skip 기록만)

**환경** (2026-08-27 갱신):
- `rustup` 설치 완료, `cargo 1.98.0`(`rustc 1.98.0`) 사용 가능
- Cargo 워크스페이스 시드 완료: `crates/crew-harness`, `crates/crew-proto` (아직 스텁)
- 설치된 CLI: `claude`(/opt/homebrew/bin), `opencode`(/opt/homebrew/bin)
- 미설치: `codex`, `gemini`, `grok`, `cursor-agent`, `amp`, `qwen`
- 이 저장소는 **git init 완료** (`crew/t-docs` 등 태스크 브랜치로 오케스트레이션 진행 중)

**이번 런 진행 상황**: M3 오케스트레이션 런(8태스크, run-id m3a) 완료 — 전부 `main`에
머지됨(cac9a13). 런 기록(계약·플랜·리뷰·에스컬레이션)은 `archive-20260827-m3a/` 참조.
특히 `archive-20260827-m3a/contracts-m3.md`가 M3 봉투 body 계약의 정본이었다
(task.assign body=`{"task": TaskSpec}`, task.result body=`{"covered_req_ids","artifacts"}` 인밴드).

---

## 4. 다음 스텝 — M4 UI

M1(하네스)·M2(버스+왕복)·M3(Lead 오케스트레이션)는 완료·실측 검증됨(§3). 다음은 M4.

**M4 — UI** (DESIGN.md §11):
- 스프린트 보드 + 라이브 스레드 + 에이전트 레일 (DAG/타임라인은 그다음).
- ✅ 성공 기준: 실행 중인 런을 앱만 보고 완전히 이해 가능.

**M4 착수 전 결정 2건** (§2 미결, DESIGN §12에 추천안 있음): ① 프로젝트 이름
(`agent-crew`는 가칭) ② Tauri 내부 프론트엔드(React/Svelte/순수 TS).

**M4 선행 배선 작업** (테스트에만 존재하는 조립을 앱 코드로 승격):
1. **런 컨트롤러**: 현재 버스+원장+Lead+5워커 조립은 `crates/crew-lead/tests/m3_sprint.rs`
   안에만 있다 — 이를 재사용 가능한 런 실행기(크레이트 or `crew-app`)로 추출. Tauri 커맨드가
   이걸 호출한다.
2. **UI 데이터 소스**: `crew-ledger`(SQLite, `BusEvent` 원장)와 `BusHandle::subscribe()`
   브로드캐스트가 라이브 스레드/에이전트 레일의 소스. 원장에는 봉투 kind가 없으므로 UI가
   메시지 수준 표시를 하려면 원장 스키마 확장(DESIGN §8 방향) 또는 봉투 스트림 병행 구독 필요.
3. **Tauri 2 셸**: 워크스페이스에 Tauri 앱 크레이트 추가 (§2 스택 결정 준수).
   `LeadPlanner::specify`의 LLM 교체(§4.3)는 M4 범위 아님 — 결정론 템플릿 그대로 사용.

M3에서 검증된 계약을 그대로 소비할 것: `AgentRunner`/`RoleBehavior`/`BusConn`,
`LeadBehavior::new(agent_id, dag, sprint, roles, max_rework)`, `EventLedger`/`spawn_subscriber`,
`RoleHarnessBehavior`(실 CLI), `ScriptedCrewMember`(결정론 테스트).

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
