# 실런(live run) 상태에서의 시각 검증

측정 날짜: 2026-09-02
베이스: t1-live-events(머지됨) + t3-font-600(머지됨)가 적용된 워크트리.
엔진: `npm run tauri dev` 실앱(WKWebView, macOS) — `TAURI-WEBVIEW-VERIFY.md`
§1과 동일 엔진.

**전제(t1이 확정, 재조사하지 않음)**: 라이브 이벤트는 emit→listen→store→
derive 전 구간 정상 도달(84/84). 스크립티드 런은 ~0.2–2.4초에 완주한다.
`TAURI-WEBVIEW-VERIFY.md` §5 참조.

## 측정 방법

WKWebView에는 CDP가 없어 `TAURI-WEBVIEW-VERIFY.md` §1과 같은 방식을 그대로
썼다: `index.html`에 스크립트 한 줄을 임시로 추가하고(`src/__verify__/
probe.ts`), 결과를 `127.0.0.1:1421` 로컬 수집 서버(`.claude/tmp/`, 저장소
밖)에 POST했다. 방법 세부는 다음과 같다.

- **순간 상태 포착(D1)**: `MutationObserver`를 `.rail-list`와 `.req-matrix`
  루트에 걸어 `.rail-badge`/`.req-matrix__cell`의 `class` 속성 변화를
  {ts, tag, className, label} 로 동기 기록. 보조로 `useRunStore.subscribe`로
  `taskStates` 참조가 바뀔 때마다 스냅샷을 기록.
- **런 종료 판정(D2)**: 고정 sleep 대신 `useRunStore.getState().finished !==
  null` 을 50ms 간격으로 폴링.
- **반복(D8)**: 같은 프로브 안에서 `startRun` → 완주 대기 → (2회까지)
  `defaultSource.stop()` → 재시작을 3회 수행해 배지 전이 시퀀스의 재현성을
  확인.
- **layout 스윕(D5)**: 3런 완주 후(메시지 17개 실데이터가 쌓인 상태) `.layout__right`
  컨테이너에 인라인 `height` 를 강제해 300/400/500/600/700/793/1000px +
  자연 높이를 스윕하고 `.panel--thread`/`.panel--roster` 의
  `getBoundingClientRect()` 와 `scrollHeight > clientHeight` 를 기록.
  (macOS 손쉬운 사용 권한이 없어 `osascript`/`screencapture` 로 실제 창
  크기를 조절할 수 없으므로 지난 런과 동일하게 컨테이너 스타일 강제로
  대체 — `TAURI-WEBVIEW-VERIFY.md` Fix 3과 동일 방법.)
- **수집기 생존 확인**: 프로브 주입 전 `curl -X OPTIONS/POST`로 왕복을
  확인했다(CORS preflight 응답 204, POST 응답 200). 최초 시도에서
  실제 웹뷰의 `fetch(..., {headers:{content-type:"application/json"}})`
  가 preflight `OPTIONS` 를 유발했는데 수집 서버가 `OPTIONS`를 처리하지
  않아 전 요청이 조용히 실패했다(probe_alive조차 도착하지 않음) — 수집기에
  CORS 헤더/OPTIONS 처리를 추가한 뒤 재측정했다. 이 실패와 수정은 수집
  서버(저장소 밖 임시 스크립트) 문제였고 앱 코드와는 무관하다.
- 프로브 주입(`index.html` 1줄 + `src/__verify__/probe.ts`)과 수집 서버는
  측정 후 전부 원복/삭제했다. **저장소에는 이 문서만 남는다.**

## 1. rail 배지 3상태 전이 타임라인

3회 스크립티드 런(`stop_run` 후 재시작) 결과, `t0`(startRun 호출) 기준
경과 ms:

| 런 | 완주 ms | 배지 | 전이 | 체류(ms) |
|---|---|---|---|---|
| 1 | 216 | Lead | idle→working @+31 → working→idle @+213 | working 182 |
| 1 | 216 | PM | idle→working @+76 → working→idle @+77 | working 1 |
| 2 | 70 | Lead | idle→working @+19 → working→idle @+67 | working 48 |
| 2 | 70 | PM | idle→working @+40 → working→idle @+41 | working 1 |
| 3 | 73 | Lead | idle→working @+23 → working→idle @+69 | working 46 |
| 3 | 73 | PM | (전이 미관측) | — |

관측된 `rail-badge` class 값은 `--working` 5건, `--idle` 5건이 전부다
(`grep -c` 로 대조). **`--awaiting`은 3런 중 단 한 번도 관측되지 않았다.**

**awaiting 자연 발생 여부 — 판정: 관측되지 않음(0/3), 죽은 상태로 단정할 수는
없음.** 소스(`src/features/rail/derive.ts:63-72`, `:86-95`)를 보면
`awaiting`은 "task state가 여전히 `assigned`이고, 그 task의 corr 메시지
로그에 `task.result` 가 있고 아직 `change_request`로 되돌려지지 않았을 때"만
성립하며, task state가 `accepted`로 바뀌는 순간 `roleCard`는 무조건
`idle`을 반환한다(§95: `if (state !== "assigned") return idle` 분기가
awaiting 판정보다 먼저 실행됨). 즉 awaiting의 존재 창은 "task.result 전송"과
"accepted로의 상태 전이" 사이로, 이 스크립티드 런에서는 그 창이 한 자릿수
ms 이하로 닫혀 `MutationObserver`가 같은 속성(class)의 중간값을 놓치고
최종값(idle)만 기록했을 가능성이 높다(네이티브 MutationObserver는 같은
target·속성에 대해 콜백 실행 전 동기적으로 여러 번 값이 바뀌면 마지막
값만 큐잉하는 동작을 한다). PM 배지가 working조차 3번째 런에서 관측되지
않은 것(working 1건도 두 런에서만 잡힘)도 같은 결의 코얼레싱 증거다.
**결론: awaiting이 프로덕션에서 죽은 상태라는 근거는 없다 — 오히려 소스상
도달 가능한 분기이며, 이 스크립티드 데모의 타이밍이 DOM 기반 순간 포착
방법의 해상도보다 빠른 것으로 보인다.** (재진단·수정은 범위 밖 — 사실만
보고.)

## 2. 커버리지 매트릭스 `none` 판정 (동적 + 정적)

**동적**: 3런에 걸쳐(런1 완주 후, 런2 전/후, 런3 전/후 — 5개 스냅샷 ×
5 req × 5 task = 125 셀) 관측된 class는:

| 상태 | 관측 횟수 |
|---|---|
| `req-matrix__cell--expected` | 125 |
| `req-matrix__cell--covered` | 0 |
| `req-matrix__cell--none` | 0 |

**`none`은 5개 requirement × 5개 task, 3런 전 구간에서 단 한 번도
관측되지 않았다.** `covered`도 3런 전 구간(125셀)에서 단 한 번도
관측되지 않았다 — 완주 후에도 전 셀이 `expected`였다. `none`과 마찬가지로
**정적 판정을 붙인다** (아래).

### `none` 정적 판정

**정적**: `crates/crew-lead/src/plan.rs:205-219`의 task 빌더는 모든 task의
`artifacts_expected[0].req_ids`를 `req_ids.to_vec()`(스펙의 **전체**
requirement id 목록)로 채운다. 즉 시나리오 템플릿상 **모든 task가 모든
requirement를 기대한다.** 이는 하드코딩된 불변식이 아니라 테스트로
못박혀 있다(`plan.rs:291`
`assert_eq!(task.artifacts_expected[0].req_ids, all_req_ids)`).

프론트 도출(`src/features/artifacts/derive.ts:120-158`,
`buildReqMatrix`)의 셀 규칙은 `covered > expected > none` 우선순위이고,
`expected`는 `task.artifacts_expected.some(a => a.req_ids.includes(req.id))`
로 판정된다. 모든 task의 `req_ids`가 전체 requirement 집합이므로 이
조건은 **항상 참**이다 — 어떤 (task, req) 조합도 `expected` 미만으로
떨어질 수 없다.

**결론(동적+정적 일치): `none`은 현재 시나리오 템플릿에서 프로덕션에
도달 불가능한(dead) 상태다.** 이미 `TAURI-WEBVIEW-VERIFY.md` Fix 2에서
mock 소스도 동일함이 확인된 바 있고(`mock-source.ts:98`), 이번 측정은
실제 Tauri/Rust 백엔드 경로에서도 같은 결론임을 재확인했다. **고치지 않고
보고만 한다.**

### `covered` 정적 판정 — 도달 불가(근거: corr 키 불일치)

**성립 조건**(`buildReqMatrix`, `src/features/artifacts/derive.ts:141-152`):
`task.result` 메시지를 순회하며 `latestCoveredByTask.set(envelope.corr,
covered_req_ids)`(:146)로 맵을 채운 뒤, 셀 판정에서
`latestCoveredByTask.get(task.id)`(:152)로 **task의 bare id**로 조회한다.
즉 이 함수는 **`envelope.corr === task.id`라는 전제**를 깔고 있다.

**스크립티드 경로가 그 전제를 만드는가 — 아니다.** 실제 Rust 백엔드
(`crates/crew-lead/src/dispatch.rs:266-268`)는 `task.assign`을 보낼 때
corr을 `format!("corr-{id}")`로 만든다(예: task id `t-pm` → corr
`"corr-t-pm"`, 테스트로 못박힘: `dispatch.rs:703`
`assert_eq!(envelopes[0].corr, "corr-t-pm")`). `ScriptedCrewMember`는 이
corr을 그대로 반사한다(`crates/crew-agent/src/crew_member.rs:36-50`
`reply()`가 `in_reply_to.corr.clone()` 사용) — 즉 `task.result`의 corr도
`"corr-t-pm"`이지 `"t-pm"`이 아니다. 프런트 `Envelope` 타입(`src/lib/
types.ts:78`)은 corr을 원문 그대로 전달하며 중간에 접두어를 벗기는
코드는 없다. 반면 **mock**(`src/lib/mock-source.ts:231` 등)은 corr을
`task.id` 그대로(접두어 없이) 쓴다 — mock에서만 이 전제가 우연히
성립한다.

**결론: `covered`는 실제 백엔드 경로에서 프로덕션 도달 불가능하다.**
`latestCoveredByTask.get(task.id)`가 항상 undefined를 반환하므로(맵의
키는 `"corr-t-pm"`류, 조회 키는 `"t-pm"`류) `covered` 분기에 절대
도달하지 못하고 `expected`로만 떨어진다 — 이번 측정에서 125/125 셀이
`expected`였던 것과 정확히 일치한다. 참고로 백엔드 자체의 DoD 판정
(`crates/crew-lead/src/dispatch.rs:314,368`)은 `env.corr.strip_prefix
("corr-")`로 접두어를 벗기고 처리하므로 **수락(accepted) 로직은
정상** — task들이 실제로 accepted까지 간 이유다. 버그는 프런트
`buildReqMatrix`의 corr 키 조회 한 곳에 있다(`derive.ts:152`).
**코드 수정은 범위 밖 — 보고만 한다.**

## 3. `.layout__right` 50/50 — 실데이터(메시지 17건) 스윕

3런 완주 후 스레드에 실제 메시지 17건이 쌓인 상태에서 측정:

| 컨테이너 높이(px) | thread(px) | roster(px) | thread 스크롤 | roster 스크롤 |
|---|---|---|---|---|
| 300 | 150 | 150 | 예 | 예 |
| 400 | 200 | 200 | 예 | 예 |
| 500 | 250 | 250 | 예 | 예 |
| 600 | 300 | 300 | 예 | 예 |
| 700 | 350 | 350 | 예 | 예 |
| 793 | 396.5 | 396.5 | 예 | 예 |
| 1000 | 500 | 500 | 예 | 예 |
| 자연(661) | 330.5 | 330.5 | 예 | 예 |

전 구간에서 정확히 50/50 분할이 유지된다(`TAURI-WEBVIEW-VERIFY.md` Fix 3과
동일 결론, 실데이터로 재확인). **스크롤 경계**: 테스트한 300–1000px 전
구간, 자연 높이(661px)를 포함해 thread·roster 모두 스크롤이 발생했다 —
즉 이번 실데이터(17건 메시지)로는 **스크롤이 발생하지 않는 높이를
1000px 이하 범위에서 찾지 못했다.** (지난 런은 mock 데이터였고 roster만
스크롤이 "예"로 보고됐던 것과 달리, 이번엔 thread도 전 구간에서
스크롤된다 — 실데이터 쪽이 콘텐츠가 더 김을 시사하는 관찰이며, 이 자체를
결함으로 판단하지 않는다.)

## 4. 재현성(D8) 메모

3런 모두 `outcome: "completed"`, task 5개 전부 `accepted`로 수렴했다.
완주 후 `messages.length`는 런1·런2 18, 런3 17로 1건 차이가 있었다(원인
미조사 — 범위 밖. 가설 한 줄: `finished` 신호와 마지막 `message` 이벤트가
서로 다른 emit이라면, `waitFinished()` 폴링이 마지막 메시지의 store 반영보다
먼저 `finished !== null`을 관측하는 경쟁이 있을 수 있다 — 검증하지 않음).
Lead 배지의 working→idle 전이는 3런 모두 관측됐지만
PM 배지 전이는 2/3런에서만 관측됐다(§1 참조, MutationObserver 코얼레싱
가설과 일치).

## 5. 결론 요약

| 항목 | 판정 |
|---|---|
| rail working/idle 자연 발생 | 예(3/3, working 182/48/46ms 등 다양한 체류) |
| rail awaiting 자연 발생 | 미관측(0/3) — 죽었다고 단정 불가, 측정 해상도 한계로 추정 |
| 매트릭스 `none` | 동적 0/125, 정적으로도 도달 불가 — **프로덕션에서 죽은 상태**(원인: 모든 task가 전체 req_ids를 기대하도록 시나리오 템플릿이 고정됨) |
| 매트릭스 `covered` | 동적 0/125, 정적으로도 도달 불가 — **프로덕션에서 죽은 상태**(원인: `buildReqMatrix`가 `envelope.corr`을 bare task id로 오인하고 조회하는데, 실제 백엔드의 corr은 `"corr-{id}"` 형식이라 항상 미스매치. mock은 corr을 bare id로 써서 이 버그를 가림) |
| `.layout__right` 50/50 | 전 구간(300–1000px) 유지, 깨짐 없음 |
| threadScrollable 경계 | 1000px 이하 범위에서 미발견(전 구간 스크롤) |

## 프로브 원복

측정 후 `index.html`의 임시 스크립트 태그와 `src/__verify__/`를
제거했다. `.claude/tmp/t2-live-visual-verify/`(수집 서버·원시 JSON,
저장소 밖)도 삭제했다. `git status` 에는 이 문서(및 `TAURI-WEBVIEW-VERIFY.md`
갱신)만 남는다.

## 6. t4-covered-corr-fix 후속 — `covered` 수정 확인 (2026-09-02)

**수정**: `src/features/artifacts/derive.ts`의 `buildReqMatrix`/`buildArtifactIndex`가
`envelope.corr`을 bare task id로 오인하던 것을, board/rail derive와 동일하게
corr을 불투명 토큰으로 취급하도록 고쳤다 — `task.assign` 메시지의
`body.task.id`에서 corr→taskId 맵을 만들고(`buildCorrToTaskId`), `task.result`의
corr을 이 맵으로 해석한다. 맵에 없는 corr(assign 없이 result만 오는
mock/테스트 경로)은 corr 값 그대로 폴백해 기존 mock 동작을 보존한다. 파생
시맨틱(`covered > expected > none` 우선순위 등)은 변경하지 않았다.

**실측**: 위 §2와 동일한 방법(`TAURI-WEBVIEW-VERIFY.md` §1, index.html 1줄 +
`src/__verify__/probe.ts`, `127.0.0.1:1421` 수집 서버)으로 수정 후
`npm run tauri dev` 실런 1회를 완주까지 실행해 `buildReqMatrix`/
`buildArtifactIndex` 결과를 직접 측정했다(1회 시도만에 메시지 18건 수신,
D8 재시도 루프 불필요):

| 항목 | 수정 전(§2, 3런 125셀) | 수정 후(1런 25셀) |
|---|---|---|
| `covered` | 0 | **25** |
| `expected` | 125 | 0 |
| `none` | 0 | 0 |
| `buildArtifactIndex` taskId | `"corr-t-pm"`류(오염) | `t-pm` 등 bare id(정상) |

셀 5개 requirement × 5개 task = 25셀 전부가 `covered`로 전환됐고,
`buildArtifactIndex`가 반환한 `taskId`도 전부 `corr-` 접두어 없는 bare task
id였다(`["t-pm","t-design","t-publish","t-dev","t-qa"]`). `npx vitest run`은
197/197 통과(기존 193 + 신규 4: `buildArtifactIndex`/`buildReqMatrix` 각
corr 해석 케이스 + 폴백 경계 케이스).

**부수 관찰(범위 밖, 코드 무변경, 보고만)**: 측정 중 `run_snapshot`
직후 재시작 없이 진행한 첫 시도에서 `finished`/`taskStates`는 정상 도달했지만
`messages`가 0건인 경우를 관측했다(task.assign/result 포함 전 종류
메시지가 0건 — §4의 "메시지 1건 차이" 레이스보다 심한 사례). `TauriEventSource.start()`의
스냅샷 replay(`snapshot.messages`)와 라이브 이벤트 버퍼링(`lastSeq` 게이팅)
사이에 스크립티드 런(70–220ms 완주)이 워낙 빨라 생기는 것으로 보이는
경합으로 추정되며, D8과 동일하게 `defaultSource.stop()` → 재시작으로
회피 가능했다(1차 재시도에서 메시지 18건 정상 수신). `src-tauri/**` 무변경
원칙(t4 범위 밖)에 따라 코드 수정은 하지 않았다 — 다음 라이브 검증
작업자를 위해 여기 기록만 남긴다.

## 7. t1-msg-race 후속 — messages 0건 레이스 근본 수정 확인 (연속 10런, 2026-09-02)

**배경**: §6 "부수 관찰"에서 보고된 `messages` 0건 레이스(`run_snapshot`의
`last_seq`가 실제로 반환된 `messages`와 독립적으로 계산되어, 프론트
`lastSeq` 게이팅이 라이브 메시지 버퍼 전체를 "이미 재생됨"으로 오판하고
드롭하는 결함)를 근본 수정했다. 수정: `crates/crew-run/src/controller.rs`의
`RunHandle::snapshot()`이 `last_seq`를 `snap.last_seq`가 아니라 **실제
반환하는 `messages`의 max seq(빈 배열이면 0)로 계산**하도록 바꿔 구성상
원자적으로 만들었고(`snapshot_messages_and_last_seq`), 프론트
`tauri-source.ts`의 `start()`도 `this.lastSeq`를 `snapshot.last_seq`가
아니라 **실제 재생한 messages의 max seq**로 계산하도록 방어적으로 고쳤다.
재현·수정 전 과정은 TDD로 못박혔다 — Rust 쪽은 `snapshot_atomicity_tests`
모듈에 스레드+`Barrier` 강제 인터리브(ledger commit이 `SnapshotState.
last_seq` 갱신보다 먼저 일어나는 정확한 경합 창)로 불변식
(`last_seq == messages의 max seq`)이 수정 전엔 깨지고(RED, `left: 0,
right: 1`) 수정 후엔 항상 성립함을(GREEN) 확인했다. TS 쪽은
`tauri-source.test.ts`에 `{last_seq: 5, messages: []}` 스냅샷 + 라이브
seq 1..5 버퍼 시나리오를 추가해 수정 전 0건 전달(RED)→수정 후 5건 전체
전달(GREEN)을 확인했다.

**측정 방법**: §1/§6과 동일한 프로브 방법(`TAURI-WEBVIEW-VERIFY.md` §1,
`index.html` 1줄 + `src/__verify__/probe.ts`, `127.0.0.1:1421` 수집
서버 — OPTIONS/CORS preflight 처리 포함)을 그대로 썼다. 프로브는 앱
마운트 후 자동으로 `useRunStore.getState().startRun(goal)` → `finished
!== null` 폴링(50ms 간격, 고정 sleep 아님, 30초 상한) → 결과 리포트 →
`defaultSource.stop()` → 재시작을 **10회 연속** 반복했다. `npm run tauri
dev` 실앱(WKWebView, macOS)에서 실행했다. 측정 후 프로브 주입과 수집
서버는 전부 원복/삭제했다(아래 "프로브 원복" 참조).

**결과**: 10런 전부 `outcome: "completed"`, 전부 `messageCount: 17`
(손실 0/10), 전부 `hasAssign: true` / `hasResult: true`(`task.assign`·
`task.result` 포함 확인). 완주 시간은 67–164ms(런1이 164ms로 가장
길었고 이후 67–77ms로 수렴 — 첫 런의 JIT/캐시 워밍업으로 추정, 판정에
영향 없음).

| 런 | outcome | messageCount | hasAssign | hasResult | elapsedMs |
|---|---|---|---|---|---|
| 1 | completed | 17 | true | true | 164 |
| 2 | completed | 17 | true | true | 75 |
| 3 | completed | 17 | true | true | 69 |
| 4 | completed | 17 | true | true | 69 |
| 5 | completed | 17 | true | true | 67 |
| 6 | completed | 17 | true | true | 77 |
| 7 | completed | 17 | true | true | 77 |
| 8 | completed | 17 | true | true | 72 |
| 9 | completed | 17 | true | true | 70 |
| 10 | completed | 17 | true | true | 70 |

10런 모두 동일한 17개 `kinds` 시퀀스(`task.ack`/`task.result`/
`task.assign`/`change_request` 조합)를 보여 실행별 편차 없이 재현성이
있다.

**결론**: §6에서 보고된 `messages` 0건 레이스는 연속 10런에서 재발하지
않았다 — 수정이 실제 WKWebView/Rust 백엔드 경로에서 유효함을 확인했다.

**프로브 원복**: 측정 후 `index.html`의 임시 스크립트 태그, `src/
__verify__/`, 수집 서버(`.claude/tmp/t1-msg-race-live-verify/`, 저장소
밖 취급 — `.git/info/exclude`로 무시됨)를 전부 제거했다. `git status`에는
이 문서 갱신과 `crates/crew-run/src/controller.rs` /
`apps/crew-app/src/lib/tauri-source.ts` / `apps/crew-app/src/lib/
tauri-source.test.ts`(수정 자체)만 남는다.

## 8. PR #11 실런 시각 검증 — 매트릭스 none 셀 + awaiting min-hold (lf3, 2026-09-02)

**배경**: PR #11(main `807dfbb`) 변경분의 실런 시각 검증 중 lf2가 테스트·
통합 리뷰까지 닫고 범위 밖으로 남긴 실앱 육안/실측 확인 1건. 확인 대상:
(1) requirements 매트릭스 none 셀 5개(design×REQ-5, publish×REQ-4,
dev×REQ-1,2,5) 렌더, (2) awaiting("응답대기") 배지 ≥600ms 가시성(lf1에서
0/3 미관측이었던 항목).

**측정 방법**: §1/§7과 동일한 프로브 방법(`TAURI-WEBVIEW-VERIFY.md` §1,
`index.html` 1줄 + `src/__verify__/probe.ts`, `127.0.0.1:1421` 수집
서버 — OPTIONS/CORS preflight 처리 포함)을 그대로 썼다. 수집기 생존은
`curl -X OPTIONS`(204)/`curl -X POST`(200) 왕복 확인 후 실웹뷰의
`probe_alive` POST 도착으로 재확인했다(`userAgent`: WKWebView, Tauri).

프로브는 마운트 후 "아티팩트" 탭을 클릭해 매트릭스를 마운트 상태로 유지한
채 세 가지를 계측했다:

- **매트릭스 스냅샷(D4)**: 각 런 완주(`useRunStore.getState().finished
  !== null`, 50ms 폴링, 30초 상한) 직후 `.req-matrix__cell` 25개 전부에
  대해 `aria-label`로 reqId/taskId/state를 식별하고
  `className`/`getBoundingClientRect()`/`getComputedStyle`의
  `backgroundColor`·`borderColor`·`color`를 기록.
- **배지 타임라인(D5)**: MutationObserver 대신 rAF 루프에서 매 프레임
  `.rail-card`마다 `.rail-badge`의 `className`을 이전 프레임과 비교해
  변화 시점만 `{ts, role, className}`로 기록. 런 시작 전부터 마지막 런
  완주 후 3초까지 지속.
- **store 레벨 교차검증(추가)**: `useRunStore.subscribe`로 zustand
  스토어의 원시 변경을 React 렌더와 무관하게 직접 관측 — 매 변경마다
  `deriveRail(...)`을 재계산해 idle이 아닌 상태로 바뀔 때만
  `{ts, id, role, status}`를 기록. DOM이 아무것도 못 그렸어도 데이터
  계층에서 transient 상태가 실재했는지 판별하기 위함.

스크립티드 런 3회 연속(`stop_run` 후 재시작, D7)을, 아래 §2의 CSS 수정
전후로 각각 수행했다(수정 전 2세트, 수정 후 1세트 — 총 9런). 프로브
주입과 수집 서버는 측정 후 전부 원복/삭제했다(§4 참조).

### 1. 커버리지 매트릭스 none 셀 (5/5, 3세트 9런 전부 일치)

9런(수정 전 6런 + 수정 후 3런) 전부 25셀(5 REQ × 5 task) 스냅샷에서
none 셀이 정확히 다음 5개로 관측됐다:

| REQ × Task | 상태 |
|---|---|
| REQ-1 × t-dev | none |
| REQ-2 × t-dev | none |
| REQ-4 × t-publish | none |
| REQ-5 × t-design | none |
| REQ-5 × t-dev | none |

브리프에 명시된 기대 5셀(design×REQ-5, publish×REQ-4, dev×REQ-1,2,5)과
정확히 일치 — 9런 모두 편차 없음.

### 2. none 셀 시각 구분 — 결함 발견 및 수정(CSS specificity)

**최초 관측(수정 전, 2세트 6런)**: 매트릭스 스냅샷에서 covered/expected/
none 세 상태 전부 동일한 `borderColor`(`oklch(0.27 0.014 215)` =
`--border-hairline`)로 렌더됐다.

| 상태 | backgroundColor | borderColor(수정 전) |
|---|---|---|
| covered | `oklch(0.72 0.15 150)` | `oklch(0.27 0.014 215)` |
| expected | `oklch(0.19 0.012 215)` | `oklch(0.27 0.014 215)` |
| none | `rgba(0, 0, 0, 0)`(투명) | `oklch(0.27 0.014 215)` |

**원인**: `artifacts.css`의 `.req-matrix__cell--none { border-color:
var(--surface-panel); }`(specificity 0,1,0)이 `.req-matrix th,
.req-matrix td { border: 1px solid var(--border-hairline); }`
(specificity 0,1,1)보다 소스 순서와 무관하게 항상 낮아 — 의도한
오버라이드가 한 번도 적용되지 않았다. 결과: none과 expected의 유일한
구분 수단이 background뿐인데(투명 → body `--surface-paper`(13%) vs
`--surface-raised`(19%)), 이는 `TAURI-WEBVIEW-VERIFY.md` Fix2가
"수정함"으로 기록한 대비비 1.09(육안 구분 불가) 문제가 실제로는 재발한
상태였다 — border 기반 보정이 CSS 캐스케이드에서 무력화됐기 때문.

**수정(사용자 승인, 이번 런 범위)**: `.req-matrix__cell--none` 선택자를
`.req-matrix td.req-matrix__cell--none`(specificity 0,2,1)로 좁혀
`.req-matrix td` 규칙(0,1,1)을 확실히 이기도록 했다. 토큰 값·background
규칙·covered/expected·min-width는 변경하지 않았다.

**수정 후 실측(재측정, 1세트 3런 전부 일치)**:

| 상태 | backgroundColor | borderColor(수정 후) |
|---|---|---|
| covered | `oklch(0.72 0.15 150)` | `oklch(0.27 0.014 215)` |
| expected | `oklch(0.19 0.012 215)` | `oklch(0.27 0.014 215)` |
| none | `rgba(0, 0, 0, 0)`(투명) | `oklch(0.16 0.012 215)` |

`oklch(0.16 0.012 215)`는 `--surface-panel`(`--elev-1: 16%`, chroma
0.012, hue 215)의 해석값과 정확히 일치 — 오버라이드가 이제 적용된다.
none의 border가 expected/covered와 명확히 다른 값으로 분리됐다(의도한
"테두리가 배경 쪽으로 낮아져 격자에서 빠진다"는 시맨틱이 실제로 성립).

**회귀 가드**: `src/test/css-integrity.test.mjs`에 순수 CSS specificity
계산기(`specificity()` — id/class·pseudo-class/type 카운트, CSS
cascade 명세 기준)와 CSS 규칙 파서(`parseRules()`)를 추가하고,
`artifacts.css`에서
실제 두 규칙의 선택자를 파싱해 none 오버라이드가 공유 `td` 규칙보다
specificity상 확실히 앞서는지 단정하는 테스트를 추가했다(jsdom의 CSS
캐스케이드 엔진에 의존하지 않는 순수 구문 분석이라 신뢰 가능 — jsdom
자체는 실제 브라우저만큼 캐스케이드 해석을 보증하지 않으므로 실측은
라이브 웹뷰에 의존했고, 이 테스트는 "이 규칙이 다시 낮은 specificity로
회귀하면" 잡아내는 정적 가드다).

**감사에서 잡힌 자체 결함**: 최초 구현은 선택자를 `/^([^{]+)\{/m` 정규식
(`}` 경계 없음)으로 추출했는데, `test-quality-auditor` 서브에이전트가
이 정규식이 이전 규칙·주석까지 걸쳐 매칭돼(이 파일의 none 규칙 바로
위에 있는 한국어 설명 주석 포함) 잘못된 selector 문자열을 만들고, 그
잘못된 문자열의 specificity 가 우연히 기준을 통과해 **수정 전 버그가
있는 CSS 에서도 테스트가 그린으로 나오는 것**을 잡아냈다(수정 전
선택자로 되돌려 직접 재현·확인함). 주석을 먼저 제거하고 `{`/`}` 로
규칙을 명확히 경계 짓는 `parseRules()`로 교체해 고쳤다 — 고친 뒤 다시
`.req-matrix__cell--none`(수정 전 선택자)로 되돌려 이 테스트 **하나만**
실패함을 확인한 뒤 원복했다(다른 19개 테스트는 영향 없음). 기존 테스트는
손대지 않았다.

### 3. awaiting/working 배지 min-hold — 하네스(harness) 타이밍 한계(결함 아님)

**결과**: 3세트(9런) 전부에서 DOM(rAF 폴링, 프레임당 `.rail-badge`
className 비교)은 working/awaiting 배지를 단 한 번도 관측하지
못했다(모든 `badge_change` 기록이 `--idle`). 반면 zustand store를 React
렌더와 무관하게 직접 구독해 `deriveRail()` 출력을 재계산하는
교차검증에서는 "working"이 세 세트 모두에서 lead·pm 카드에 짧게(수 ms)
실재로 관측됐다. "awaiting"은 store 레벨에서도 9런 전부 0회.

마지막 세트(수정 후, 3런)의 대표값:

| 런 | 완주 ms | store 레벨 관측(working) | DOM 배지 관측 |
|---|---|---|---|
| 1 | 66 | lead @+62ms | 없음(전부 idle) |
| 2 | 127 | lead @+27ms, pm @+28ms | 없음(전부 idle) |
| 3 | 76 | pm @+19ms, lead @+19ms | 없음(전부 idle) |

**원인 분석**(`useMinHold.ts` + 그 테스트, `derive.ts` 인용):

1. **awaiting이 store 레벨에서도 0/9인 이유**: `derive.ts:65-73`의
   `isAwaitingResult()`는 "corr 내 최후 이벤트가 `task.result`"일 때만
   awaiting을 성립시키는데, 스크립티드 하네스
   (`crates/crew-agent/src/crew_member.rs:96` `ack_and_result` —
   `MessageKind::TaskAssign` 수신 즉시, 같은 호출에서 인위적 지연 없이
   `task.ack`+`task.result`를 함께 회신)와 `crew-lead/src/dispatch.rs`의
   즉시 accept 처리로 인해 태스크 상태가 `Assigned`→`Accepted`로 사실상
   같은 처리 틱 안에서 넘어간다. `derive.ts:91`의 `if (state !==
   "assigned") return idle` 분기가 awaiting 판정보다 먼저 실행되므로,
   "task.result 도착"과 "accepted로의 상태 전이" 사이 창이 실측상 폭이
   0에 가깝다 — awaiting이 코드상 도달 불가능한 게 아니라, 이 스크립티드
   하네스의 응답 지연이 사실상 0이라 그 창이 열리지 않는다.
2. **working은 store 레벨에서 실재하지만 DOM에 전혀 반영되지 않는
   이유**: `src/main.tsx`는 `createRoot`(React 18)를 쓴다 — React 18은
   네이티브 이벤트/Promise/타이머를 포함해 자동 배칭(automatic batching)
   을 전면 적용한다. 스크립티드 런의 연쇄 이벤트(task.assign→task.ack→
   task.result→상태변경 방송)가 거의 동시에(수 ms 이내) 도착해 zustand의
   개별 `set()` 호출들이 React의 같은 배치 안에 묶이면, `Rail`
   (`index.tsx:44`, `useMinHoldCards(deriveRail(...))`)이 실제로
   커밋·페인트하는 렌더는 그 배치의 **최종 값**뿐이다 — 배치 중간에
   존재했던 "working" cards 배열은 `useMinHoldCards`에 prop으로 전달되는
   순간 자체가 없다. `useMinHold.ts`의 hold 로직은 자신이 **받는**
   transient 값을 최소 600ms 유지하도록 정확히 동작한다(단위테스트로
   증명됨: `useMinHold.test.ts`의 "holds a transient status (awaiting)
   for holdMs before dropping to the latest status"[27행], "coalesces
   multiple status changes during a hold into the latest status at
   expiry"[48행], "starts a new chained hold if the status is transient
   again right at expiry"[70행]) — 하지만 애초에 그 값을 한 번도 못
   받으면 hold 타이머 자체가 시작되지 않는다. 이 단위테스트들은 이벤트가
   서로 몇 초씩 떨어져 도착하는(각자 별도 React 커밋을 만드는) 실제
   시나리오에서는 hold가 정상 작동함을 뒷받침한다 — fake timer로 각 상태
   변화를 개별 `rerender()` 호출로 분리해 놓고 hold가 정확히 600ms 뒤에
   만료됨을 검증하기 때문이다.
3. **결론**: `derive.ts`/`useMinHold.ts`는 각각 단위테스트가 증명하는
   대로 정확히 동작한다 — 결함은 두 컴포넌트 어디에도 없다. 이것은
   "스크립티드 데모 하네스가 사람이 인지하기엔 물론이고 React의 자동
   배칭 경계보다도 빠르다"는 **하네스 타이밍 특성**이며,
   `TAURI-WEBVIEW-VERIFY.md §5`("결론: 결함이 아니라 의도된 동작")·
   `LIVE-RUN-VERIFY.md §1`(0/3 미관측, MutationObserver 코얼레싱 가설)의
   결론과 일치·확장한다 — 이번 측정은 MutationObserver뿐 아니라 rAF
   폴링도, 심지어 React 배칭 자체가 중간 렌더를 아예 커밋하지 않는 한
   관측할 수 없음을 store-subscribe 교차검증으로 처음 확증했다. 실제
   프로덕션(느린 진짜 LLM 백엔드, 이벤트 간 간격이 초 단위)에서는 각
   이벤트가 별도 React 커밋을 만들 가능성이 높아 hold가 정상적으로
   관측 가능할 것으로 추정되나, 이는 이번 스크립티드 하네스로는 검증할
   수 없다(범위 밖 — 앱 코드·하네스 수정 없이 사실만 보고).

**결정**: 코디네이터 승인에 따라 알려진 하네스 한계(known limitation)로
기록하고 완료 처리한다. `derive.ts`/`useMinHold.ts`/스크립티드 하네스
수정은 하지 않았다.

### 4. 프로브 원복

측정 후 `index.html`의 임시 스크립트 태그, `src/__verify__/`, 수집
서버(`.claude/tmp/lf3-visual-verify/`, 저장소 밖 취급 —
`.git/info/exclude`로 무시됨)를 전부 제거했다. `git status`에는 이 문서
갱신과 `apps/crew-app/src/features/artifacts/artifacts.css`(none 셀
border-color specificity 수정) / `apps/crew-app/src/test/
css-integrity.test.mjs`(회귀 가드 추가)만 남는다.

### 5. 회귀 확인

- `npx vitest run`: **213 pass**(기존 206 + 이번에 추가한 specificity/
  parseRules 계산기 검증 6개 + none-cell 회귀 가드 1개)
- `cd apps/crew-app/src-tauri && cargo test --lib`: **15 pass**
