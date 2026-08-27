# RESEARCH — 외부 근거 조사 (2026-08-27)

> t-docs 태스크 산출물. DESIGN.md/HANDOFF.md를 뒤집지 않고 **외부 근거로 보강**하기 위한 조사.
> 모든 주장에 `[R-n]` 태그를 달았고, 출처는 문서 하단 표에 URL·조회일과 함께 정리했다.
> 확인하지 못한 것은 추측하지 않고 "확인된 자료를 찾지 못함"으로 명기한다.

---

## 1. 오케스트레이터-워커 패턴의 프로덕션 함정 (컨텍스트 폭발·비용)

- 오케스트레이터-워커 구조에서 **오케스트레이터 자체가 단일 실패점**이 된다 — 태스크를 잘못
  분류해 엉뚱한 워커에게 보내는 오분류가 스케일이 커질수록 누적된다 [R1].
- **컨텍스트 윈도우 초과**는 더 은밀한 문제로 지적된다 — 오케스트레이터가 모든 워커와의
  상호작용 전체 히스토리를 동시에 들고 있어야 하는 구조이기 때문 [R1][R2].
- 워커가 4개 이상으로 늘어나면 오케스트레이터가 "각 워커 상호작용의 전체 대화 히스토리를
  동시에 보유"해야 해서 컨텍스트 한계를 자주 넘긴다는 구체적 관찰이 있다 [R2].
- **비용 폭증** 사례: 테스트 단계에서 $0.50이던 워크플로우가 프로덕션 스케일(10만 요청)에서
  월 $50,000까지 뛴 사례가 보고됨 — 출처가 명시한 수치 그대로 인용 [R2].
- Anthropic 자체 멀티에이전트 시스템(§2 참고)도 "에이전트는 채팅 대비 약 4배, 멀티에이전트
  시스템은 채팅 대비 약 15배 더 많은 토큰을 쓴다"고 명시 — 이는 agent-crew처럼 여러 에이전트를
  상시 세션으로 띄우는 설계가 토큰 소비 면에서 근본적으로 비싸다는 뜻 [R3].

## 2. Anthropic Multi-Agent Research System 레슨

Anthropic 공식 엔지니어링 블로그(1차 출처) 기준 [R3]:

- **아키텍처**: 리드 에이전트가 조율하고 전문화된 서브에이전트가 병렬로 위임받는
  오케스트레이터-워커 패턴 — agent-crew의 Lead/에이전트 구조와 동일 패턴.
- **성능**: Opus 4 리드 + Sonnet 4 서브에이전트 조합이 단일 에이전트(Opus 4) 대비
  연구 과제에서 **90.2%** 성능 우위 — 원문 수치 그대로 인용 [R3].
- **토큰 비용**: "에이전트는 채팅 대비 약 4배, 멀티에이전트 시스템은 채팅 대비 약 15배 더
  많은 토큰을 쓴다" — 원문 그대로. (참고: 15÷4 ≈ 3.75배가 멀티에이전트가 단일 에이전트
  대비 추가로 쓰는 배수로 계산되지만, 이 3.75배 자체는 원문 수치가 아니라 두 원문 수치의
  단순 나눗셈이므로 인용이 아닌 계산값임을 밝힌다.) [R3]
- **관찰된 실패 모드** (원문 사례 그대로):
  - 단순 질의에 서브에이전트 50개를 스폰하고 존재하지 않는 소스를 끝없이 웹에서 찾아 헤맴.
  - 서브에이전트들이 태스크를 잘못 해석하거나 다른 에이전트와 완전히 동일한 검색을 중복 수행
    (예: 한 팀은 2021년 칩 위기를, 다른 팀들은 2025년 공급망을 각각 검색).
  - 이미 충분한 결과가 있는데도 계속 작업을 이어감(종료 판단 실패).
  - 초기 에이전트들이 학술 PDF 같은 권위 있는 출처보다 SEO 최적화된 콘텐츠 팜을 일관되게 선택. [R3]
- **컨텍스트 관리**: 서브에이전트가 작업 단계를 요약해 외부 메모리에 핵심 정보를 저장한 뒤
  진행하며, 컨텍스트 한계(20만 토큰 초과 시 truncate)에 도달해도 저장된 계획을 메모리에서
  복구해 이전 작업을 잃지 않는다. 서브에이전트는 결과물을 외부 시스템에 저장하고 가벼운
  참조만 리드에게 돌려준다 — agent-crew DESIGN.md §5의 "핸드오프 팩"·아티팩트 인덱스
  설계와 일치하는 방향 [R3].
- **프로덕션 난점**: 에이전트가 장시간 상태를 유지하며 많은 툴콜을 거치기 때문에, 완화 장치
  없이는 사소한 시스템 오류가 치명적으로 번질 수 있다. 한 단계 실패가 완전히 다른 궤적의
  탐색으로 이어져 예측 불가능한 결과를 낳을 수 있다. 리드 에이전트가 서브에이전트 세트를
  동기적으로 실행하고 기다리는 구조는 조율은 단순하지만 정보 흐름에 병목을 만든다 [R3].

## 3. `claude -p` stream-json 세션 관리·검증 관행

- DESIGN.md §2.2가 지적한 "`claude -p`는 실패해도 `subtype:"success"`를 뱉는다"는 문제와
  같은 맥락의 사례가 Claude Code 공식 에러 레퍼런스에도 있다 — 헤드리스 모드에서 auto 모드
  분류기 트랜스크립트가 컨텍스트 윈도우를 초과하면 에러 결과를 반환하지만 **런은 계속
  진행된다**고 명시되어 있어, 에러 신호를 놓치면 오케스트레이터가 실패를 못 보고 지나칠 수
  있음을 뒷받침한다 [R4].
- 헤드리스 실행에서 세션 이어가기 패턴: JSON 실행 결과에서 세션 ID를 캡처한 뒤 그 ID로
  `resume`하는 방식이 문서화된 패턴으로 소개된다. `--max-turns` 초과, 10MB stdin 오버플로,
  `claude auth status` 등에서 **0이 아닌 종료 코드**로 실패를 알리는 것이 문서화된 동작이며,
  개별 명령이 문서화한 것 이상으로 특정 종료 코드를 가정하지 말라는 권고가 있다 [R5].
- 실무 스크립트 예시(비공식 블로그)는 `--output-format json` 결과에서 `result`, `session_id`,
  `total_cost_usd` 필드는 안정적으로 문서화되어 있다고 소개하되, stream-json 종료 이벤트의
  `is_error` 플래그·턴 카운트·duration 등 추가 필드는 "공식 문서에 필드 단위로 완전히
  열거되어 있지 않다"고 명시하며, 파서가 이 필드들의 존재를 가정하지 말고 jq로 필요한
  필드만 읽고 누락 시 기본값을 두라고 권고한다 — DESIGN.md §2.2의 `is_error`/`terminal_reason`
  판정 로직을 짤 때 방어적으로 파싱해야 한다는 근거가 된다 [R6].
- stream-json 입력 모드 자체의 안정성 이슈(행업)가 공식 GitHub 이슈 트래커에도 보고되어
  있어, 상시 세션을 가정하는 설계(§2.1)는 프로세스 헹업/크래시에 대한 재시작·재개
  전략(DESIGN.md §9의 "CLI 프로세스 크래시" 체크리스트)이 실제로 필요함을 뒷받침한다 [R6][R4].
- 확인된 자료 없음: stream-json 종료 이벤트의 `is_error`/`terminal_reason` 필드에 대한
  Anthropic 공식 필드 스펙 문서는 이번 조사 범위에서 찾지 못했다. DESIGN.md §2.2의 서술은
  "이 맥북에서 확인"한 실측이라고 명시되어 있으므로 이 조사로 반박하거나 대체하지 않는다.

## 4. 동일 구독 다중 세션 레이트리밋 대응

- Anthropic 공식 고객센터 문서는 "현재 세션(Current session)"이 플랜의 5시간 세션 한도 중
  얼마를 썼는지, 세션 리셋까지 남은 시간을 보여준다고 안내한다 — 즉 **레이트리밋은 세션
  단위가 아니라 계정/플랜 단위 5시간 창**으로 집계된다는 공식 근거 [R7].
- 제3자 정리 자료들은 "Claude.ai, Claude Code, Claude Desktop 사용량이 동일한 사용량
  한도로 집계된다"고 설명한다 — 여러 클라이언트/세션을 동시에 띄워도 **같은 풀을
  공유**한다는 뜻이며, DESIGN.md §2.4의 "하네스별 동시성 세마포어"가 필요하다는 전제와
  일치한다 [R8][R9].
- Claude Code 전용 정리 자료(비공식)는 "여러 개의 동시 Claude Code 세션(worktree 모드)을
  한 주 내내 돌리면 주간 Opus 한도에 걸릴 수 있다"고 관찰을 보고한다 — agent-crew가 팀원
  5개 슬롯을 전부 같은 계정의 claude-code로 채우는 기본 시나리오(§2.4)에서 실제로 벌어질
  수 있는 상황과 정확히 일치하는 사례 [R10].
- 확인된 자료 없음: "하네스별 동시 세션 몇 개까지가 안전한가"에 대한 Anthropic 공식 수치
  가이드는 찾지 못했다. 공식 문서는 정확한 한도 수치를 공개하지 않는다고 명시한다 — 즉
  "숫자를 인용하는 페이지가 있다면 지어낸 것"이라는 취지의 서술도 있었다 [R9]. 따라서
  DESIGN.md에 구체적인 동시성 상한값(예: "claude-code: max 3 concurrent")을 못 박기보다
  런타임에 429/한도 응답을 관측해 적응적으로 세마포어를 조절하는 편이 안전하다는 보강
  제안의 근거로 삼는다.

## 5. 유사 OSS — 멀티 CLI 에이전트 오케스트레이터

동일 구독의 여러 CLI 코딩 에이전트(Claude Code/Codex/Gemini CLI 등)를 하나로 묶는 프로젝트가
이미 다수 존재한다. agent-crew와 문제의식이 겹치는 순서로 정리:

| 프로젝트 | 접근 | agent-crew와의 차이 |
|---|---|---|
| AWS `cli-agent-orchestrator` (awslabs) | tmux로 격리된 세션에서 Claude Code/Kiro/Codex 등을 조율 [R15] | agent-crew는 tmux 대신 자체 WS 버스 + Tauri UI로 시각화까지 포함 |
| "Parallel Code" (데스크톱 앱) | Claude Code/Codex CLI/Gemini 등 여러 에이전트를 동시 실행하는 데스크톱 앱 [R12] | 가장 근접한 선례. agent-crew는 여기에 DAG/수락기준/change_request 루프를 추가 |
| `AI-Agents-Orchestrator` (hoangsonww) | REPL/Vue-Nuxt UI로 Claude/Codex/Gemini/Copilot 조율 + 역할 기반 멀티에이전트 통신 [R13] | UI 프레임워크만 다를 뿐 역할 기반 통신 개념은 유사 |
| Emdash | Electron 기반, 34개 CLI 프로바이더 지원(Claude Code/Codex/Gemini CLI 등) [R14] | 브레드스가 넓지만(34개 CLI) DAG·수락기준 자동 진행 루프는 확인 안 됨 |
| `myclaude` (stellarlinkco) | Codex/Claude/Gemini/OpenCode 멀티 백엔드 실행, 5단계 기능 개발 워크플로 [R16] | 워크플로 고정형에 가깝고 에이전트 간 자율 협업 루프(change_request) 강조는 확인 안 됨 |
| NXTG-Forge Orchestrator | research-plan-delegate-adversarial-verify-deploy 파이프라인, 파일 락킹·드리프트 감지 포함 [R11] | 파일 락킹 개념은 agent-crew의 워크트리 분리(§6)와 목적이 같음 |

- **결론**: "여러 구독 CLI를 팀으로 묶는다"는 문제의식 자체는 이미 여러 OSS가 다루고 있어
  "유일하다"고 서술할 수 없다 — DESIGN.md에 "최초/유일" 같은 표현이 없는 것은 이 조사와
  부합한다. agent-crew의 차별점으로 조사에서 뚜렷이 확인된 것은 (a) DAG+수락기준(DoD) 자동
  진행 루프, (b) change_request 왕복 제한과 순환 감지, (c) Tauri 기반 통합 UI(스프린트
  보드+DAG+타임라인) 조합이며, 이 조합을 그대로 갖춘 선례는 이번 조사 범위에서 찾지 못했다.

## 6. 이름 후보 조사 (충돌 확인) — 5개 이상, GitHub/crates.io/Homebrew 검색

검색으로 못 찾음은 "없다"가 아니라 **"검색 범위 내 미발견"**으로만 표기한다.

| 후보 | GitHub | crates.io | Homebrew | 판정 |
|---|---|---|---|---|
| `agent-crew` (현 가칭) | GitHub Topics `agent-crew`에 CrewAI 기반 프로젝트들이 이미 다수 태깅됨, `aibtcdev/ai-agent-crew` 저장소 존재 [R17] | 검색 범위 내 미발견 | 검색 범위 내 미발견 | **충돌 위험 있음** — CrewAI 생태계와 혼동 가능 |
| `agentmux` | `markuswondrak/AgentMux`, `derai7974/AgentMux` — 둘 다 "tmux로 여러 CLI 에이전트(Claude/Codex/Gemini)를 조율"하는 **거의 동일한 개념**의 프로젝트 [R18] | 검색 범위 내 미발견 | 검색 범위 내 미발견 | **강한 충돌** — 개념까지 겹침, 제외 권장 |
| `teamforge` | `juliandunn/teamforge`, `vigneshwaranr/TeamForge-Go` 등 CollabNet TeamForge(레거시 상용 ALM 툴) 관련 저장소 다수 [R19] | 검색 범위 내 미발견 | 검색 범위 내 미발견 | **충돌** — 실존 상용 제품명과 겹쳐 혼동 소지 |
| `rolecraft` | `caliog/Rolecraft`, `MartDel/RoleCraft`, `tml1026/RoleCraft` — 전부 마인크래프트 플러그인 [R20] | 검색 범위 내 미발견 | 검색 범위 내 미발견 | 동명이인 있으나 **도메인이 완전히 다름** — 실사용 충돌 낮음 |
| `crewpilot` | 검색 범위 내 미발견 | 검색 범위 내 미발견 | 검색 범위 내 미발견 | 검색 범위 내 미발견 — 상대적으로 깨끗 |
| `crewdeck` | 검색 범위 내 미발견 | 검색 범위 내 미발견 | 검색 범위 내 미발견 | 검색 범위 내 미발견 — 상대적으로 깨끗 |
| `swarmdeck` | 검색 범위 내 미발견 | 검색 범위 내 미발견 | 검색 범위 내 미발견 | 검색 범위 내 미발견 — 상대적으로 깨끗 |

## 7. Tauri 2 프론트엔드 비교 (React vs Svelte vs 순수 TS)

| 기준 | React (+Vite) | Svelte | 순수 TS(프레임워크 없음) |
|---|---|---|---|
| ① 번들 크기/기동 시간 | 가상 DOM 오버헤드로 Svelte 대비 상대적으로 큼(정량 비교 출처는 못 찾음) [R23][R26] | 컴파일 타임 최적화로 React/Vue/Angular 대비 번들이 작은 경향이 반복 확인됨 [R22][R23][R26] | 프레임워크 런타임 자체가 없어 이론상 가장 작으나, 이번 조사에서 정량 비교 자료를 찾지 못함 — 확인된 자료 없음 |
| ② Tauri 2 IPC/이벤트 스트림 통합 | 공식 문서가 React/Vue/Svelte를 동일하게 다룸 — 난이도 차이 미확인 [R28] | 공식 문서가 React/Vue/Svelte를 동일하게 다룸 — 난이도 차이 미확인 [R28] | 공식 이벤트 API(`listen`)는 프레임워크 비의존적이라 순수 TS에서도 동일하게 동작 [R28] |
| ③ DAG·타임라인·가상 스크롤 생태계 | `xyflow`(React Flow)가 존재, 활발히 유지보수 [R27] | `xyflow`(Svelte Flow)가 React Flow와 동일 팀·동일 API로 존재 [R27] | 전용 라이브러리 생태계 비교 자료를 찾지 못함 — 확인된 자료 없음 |
| ④ 1인 개발 생산성 | Tauri 생태계에서 "가장 흔한 조합"으로 소개되어 예제·자료가 많을 가능성 시사(정량 비교 출처는 못 찾음) [R22] | React 19 컴파일러가 성능 격차를 좁혔다는 서술 있으나 생산성 정량 비교 자료는 못 찾음 [R26] | 보일러플레이트가 적어 초기 세팅 자유도는 높지만 생산성 비교 자료를 찾지 못함 — 확인된 자료 없음 |

**추천**: React + Vite — 근거는 ①④(가장 흔한 조합·예제 풍부 가능성)과 ③(xyflow DAG 생태계 검증)
[R22][R27]. **대안**: Svelte(번들 크기 이점, 단 Tauri 자체가 이미 5~10MB로 작아 체감 효과 제한적
[R24]). 이 추천은 §12 미결 6번 아래에 그대로 반영했다 — "추천"이며 "결정"이 아니다.

- **번들 크기/기동 시간**: Tauri 앱 자체는 OS 웹뷰를 재사용해 Electron 대비 5~10MB 대
  (Electron은 120MB+)로 작다는 것이 프레임워크 선택과 무관한 공통 이점 [R24]. 프레임워크
  간 비교에서는 "Svelte로 만든 것이 React/Vue/Angular 대비 번들이 작은 경향이 있다 —
  가상 DOM이 없고 최종 번들에 필요한 코드만 포함하기 때문"이라는 서술이 반복적으로
  발견된다 [R23][R22][R26].
- **Tauri 2 IPC/이벤트 스트림 통합**: 공식 문서의 `listen('event', cb)` 이벤트 API는
  React/Vue/Svelte 어디서든 동일하게 동작하며, 공식 예제도 "React/Vue/Svelte의 setup/mount
  훅이 컴포넌트가 완전히 렌더되기 전에 실행된다"는 식으로 세 프레임워크를 나란히 다룬다 —
  즉 이벤트 스트림 통합 난이도 자체는 프레임워크 간 유의미한 차이가 확인되지 않았다 [R28].
- **DAG·타임라인·가상 스크롤 라이브러리 생태계**: DAG 시각화 라이브러리 `xyflow`가
  React Flow와 Svelte Flow를 **동일 팀이 동일한 API 철학으로** 유지보수하고 있어, DESIGN.md
  §7의 DAG 뷰 요구사항 기준으로는 React/Svelte 둘 다 검증된 선택지다 [R27]. 가상 스크롤·
  타임라인 전용 라이브러리 생태계의 정량 비교(라이브러리 개수·유지보수 활성도)는 이번
  조사에서 확인된 자료를 찾지 못했다 — 확인된 자료 없음.
- **1인 개발 생산성**: React 19의 컴파일러가 자동 메모이제이션으로 수동 최적화 필요성을
  줄여 Svelte와의 런타임 성능 격차를 좁혔다는 서술이 있으나, Svelte는 애초에 "컴파일 타임에
  최적화가 끝나 프로덕션 코드가 순수 최적화된 바닐라 JS"라는 구조적 차이가 남아있다는
  서술도 함께 발견된다 [R26]. React 쪽은 "Tauri 생태계에서 가장 많이 쓰이는 조합"이라는
  서술이 있어(스타터 템플릿 기준) 예제·스택오버플로/커뮤니티 자료가 더 많을 가능성을
  시사하지만, 이 조사에서 자료량을 정량적으로 비교한 출처는 찾지 못했다 [R22].
- **Tauri 자체 대안**: 순수 TS(프레임워크 없음)는 이번 조사에서 별도로 비교한 자료를 찾지
  못했다 — 확인된 자료 없음. 다만 Tauri 공식 보일러플레이트 목록에서 React+Vite 조합이
  "생태계에서 가장 흔한 조합"으로 소개된다는 점만 확인된다 [R22].

---

## 출처 테이블

| ID | 제목 | URL | 조회일 |
|---|---|---|---|
| R1 | 6 Multi-Agent Orchestration Patterns for Production (2026) | https://beam.ai/agentic-insights/multi-agent-orchestration-patterns-production | 2026-08-27 |
| R2 | Multi-Agent Orchestration Patterns: A Practical Guide (Rost Glukhov) | https://www.glukhov.org/ai-systems/architecture/multi-agent-orchestration-patterns/ | 2026-08-27 |
| R3 | How we built our multi-agent research system (Anthropic 공식) | https://www.anthropic.com/engineering/multi-agent-research-system | 2026-08-27 |
| R4 | Error reference (Claude Code Docs 공식) | https://code.claude.com/docs/en/errors | 2026-08-27 |
| R5 | Claude Code Headless Mode (Build This Now) | https://www.buildthisnow.com/blog/guide/development/claude-code-headless-mode | 2026-08-27 |
| R6 | Claude Code in CI/CD and Headless Automation (hidekazu-konishi.com) | https://hidekazu-konishi.com/entry/claude_code_cicd_and_headless_automation.html | 2026-08-27 |
| R7 | Usage limit best practices (Anthropic Help Center 공식) | https://support.claude.com/en/articles/9797557-usage-limit-best-practices | 2026-08-27 |
| R8 | Claude Usage Limits in 2026: Five-Hour and Weekly Caps (Krater Blog) | https://krater.ai/blog/claude-usage-limits | 2026-08-27 |
| R9 | Claude Code Limits: Shared Usage Pool Explained (ClaudeLimit.com) | https://claudelimit.com/claude-code-limits/ | 2026-08-27 |
| R10 | Claude Rate Limits 2026: I Burned Through Pro & Max in One Week (heyuan110.com) | https://www.heyuan110.com/posts/ai/2026-02-28-claude-rate-limits/ | 2026-08-27 |
| R11 | awesome-agent-orchestrators (GitHub, andyrewlee) | https://github.com/andyrewlee/awesome-agent-orchestrators | 2026-08-27 |
| R12 | awesome-cli-coding-agents (GitHub, bradAGI) | https://github.com/bradAGI/awesome-cli-coding-agents | 2026-08-27 |
| R13 | AI-Agents-Orchestrator (GitHub, hoangsonww) | https://github.com/hoangsonww/AI-Agents-Orchestrator | 2026-08-27 |
| R14 | 9 Open-Source Agent Orchestrators for AI Coding (2026) (Augment Code) | https://www.augmentcode.com/tools/open-source-agent-orchestrators | 2026-08-27 |
| R15 | cli-agent-orchestrator (GitHub, awslabs) | https://github.com/awslabs/cli-agent-orchestrator | 2026-08-27 |
| R16 | myclaude (GitHub, stellarlinkco) | https://github.com/stellarlinkco/myclaude | 2026-08-27 |
| R17 | ai-agent-crew (GitHub, aibtcdev) + agent-crew GitHub Topics | https://github.com/aibtcdev/ai-agent-crew ; https://github.com/topics/agent-crew | 2026-08-27 |
| R18 | AgentMux (GitHub, markuswondrak) + AgentMux (GitHub, derai7974) | https://github.com/markuswondrak/AgentMux ; https://github.com/derai7974/AgentMux | 2026-08-27 |
| R19 | teamforge (GitHub, juliandunn) + TeamForge-Go (GitHub, vigneshwaranr) | https://github.com/juliandunn/teamforge ; https://github.com/vigneshwaranr/TeamForge-Go | 2026-08-27 |
| R20 | Rolecraft (GitHub, caliog) | https://github.com/caliog/Rolecraft | 2026-08-27 |
| R22 | Best Tauri Boilerplates 2026 (StarterPick) | https://starterpick.com/guides/best-tauri-boilerplates-2026 | 2026-08-27 |
| R23 | The Best UI Libraries for Cross-Platform Apps with Tauri (CrabNebula) | https://crabnebula.dev/blog/the-best-ui-libraries-for-cross-platform-apps-with-tauri/ | 2026-08-27 |
| R24 | Tauri v2 Tutorial 2026: Build a 5MB Desktop App (Rustify) | https://rustify.rs/articles/rust-tauri-v2-desktop-app-tutorial-2026 | 2026-08-27 |
| R26 | Svelte vs React: 7 Key Differences & Our Pick [2026] (tech-insider.org) | https://tech-insider.org/svelte-vs-react-2026/ | 2026-08-27 |
| R27 | xyflow — Node-Based UIs for React and Svelte | https://xyflow.com/ ; https://github.com/xyflow/xyflow | 2026-08-27 |
| R28 | Calling the Frontend from Rust (Tauri 2 공식 문서) | https://v2.tauri.app/develop/calling-frontend/ | 2026-08-27 |

> R21/R25는 초안 정리 과정에서 R19/R24로 병합되어 결번이다 — 동일 주장 중복 태깅을 피하기 위함.
