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
