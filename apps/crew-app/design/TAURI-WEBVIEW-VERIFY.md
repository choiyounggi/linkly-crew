# Tauri 실제 webview 검증 (macOS / WebKit)

측정 날짜: 2026-09-02
베이스 커밋: `0699fc2` (PR #8 머지 직후)
엔진: `navigator.userAgent` = `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)
AppleWebKit/605.1.15 (KHTML, like Gecko)` — Tauri 2 의 macOS WKWebView.
`isTauri()` = **true** (즉 앱은 `MockEventSource` 가 아니라 `TauriEventSource`
경로로 돌았다 — `src/lib/source.ts:34`).
뷰포트 1280×768, `devicePixelRatio` 1.

지금까지 이 앱의 모든 시각·폰트 검증은 Chromium 계열(Aside 브라우저)에서만
했다. 이 문서는 앱이 실제로 도는 엔진에서 다시 확인한 결과다.

## 측정 방법

WKWebView 에는 CDP 엔드포인트가 없어 외부에서 붙을 수 없다. 그래서 일회성
프로브를 페이지에 주입하고(`index.html` 에 `<script>` 한 줄 임시 추가), 결과를
`127.0.0.1:1421` 의 수집 서버로 POST 시켰다. **검증 후 주입은 전부 되돌렸고
저장소에는 남아 있지 않다.**

수집기 생존 확인: 매 측정마다 의도적인 `console.error` 를 같은 경로로 흘려
리포트에 그 줄이 실제로 담기는지 대조했다. 담겼다 — 따라서 "에러 0건"은
수집기가 죽어서 나온 0건이 아니다.

macOS 화면 녹화 권한이 없어 `screencapture` 는 실패했다(`could not create
image from rect`). 스크린샷 대신 **엔진이 실제로 래스터한 sRGB 값**을 캔버스
`getImageData` 로 직접 읽어 판정했다.

## 1. `.woff` 미요청 — 확인됨

`performance.getEntriesByType("resource")` 기준:

| 항목 | 값 |
|---|---|
| `.woff` 요청 | **0건** |
| `.woff2` 요청 | **9건** (mono/sans/sans-kr × 300/400/700 전부) |
| 전체 리소스 | 63건 |

woff2 만 남긴 판단은 이 엔진에서도 맞다. `.woff` 는 애초에 번들에 없고
(`dist/assets` 0개) CSS 에도 참조가 없어 요청될 경로 자체가 없다.

## 2. 한글·라틴이 IBM Plex 로 렌더되는가 — 확인됨

`getComputedStyle(...).fontFamily` 는 폴백이 일어나도 선언값을 그대로 보고하므로
쓰지 않았다. 두 가지 독립적 방법으로 판정했다.

**(a) `document.fonts`** — 9개 페이스 전부 `status: "loaded"`.
`document.fonts.check()` 도 전부 true(한글은 `"가나다라마바사아자차카타파하"`,
라틴은 `"Handgloves ABCDEFG"` 를 실제 판정 문자열로 넘겼다 — 기본
`"BESbswy"` 는 한글 페이스를 검사하지 못한다).

**(b) 캔버스 렌더 폭 대조** — 같은 문자열을 `IBM Plex …` 와 `system-ui` 로 재서
비교했다. 전부 다르다(= 폴백이 아니다).

| | plex | system-ui |
|---|---|---|
| 한글 300/400/700 | 499.52 | 484.40 |
| 라틴 sans 300 / 400 / 700 | 390.80 / 396.12 / 412.84 | 382.52 / 389.41 / 414.50 |
| 라틴 mono 300 / 400 / 700 | 432.00 (3종 동일) | 382.52 / 389.41 / 414.50 |

한글 폭이 weight 3종에서 동일한 것은 폴백 신호가 아니라 한글 활자의 성질이다
(CJK 글리프는 weight 와 무관하게 em 정폭). weight 구분 자체는 라틴 sans 쪽
폭이 300/400/700 에서 각각 다른 것으로 확인되고, 한글 각 weight 의 로드 여부는
(a) 가 독립적으로 보장한다.

## 3. 지난 런의 수정 3지점

런타임 데이터가 필요한 상태는 DOM 에서 클래스를 강제해 측정했다. Tauri 안에서는
`TauriEventSource`(실제 Rust 백엔드)가 붙는데 이 환경에서는 task 가 진행되지
않아 working/awaiting 배지와 covered/none 셀이 자연 발생하지 않았기 때문이다.
검증 대상은 데이터 파이프라인이 아니라 **이 엔진이 CSS 를 어떻게 칠하는가**
이므로 강제 적용으로 답이 나온다.

### Fix 1 — `.rail-badge--idle` 테두리 (통과)

`color-mix()` 가 이 엔진에서 정상 해석된다(WebKit 이 `oklab(… / 0.3)` 형태로
보고). 카드 배경 `oklch(0.19 …)` = rgb(14,21,23) 위에서:

| 상태 | 테두리 원시값 | 카드 위 실제 색 | 카드 대비 |
|---|---|---|---|
| idle | `oklch(0.27 0.014 215)` | rgb(31,40,43) | 1.227 |
| working | `oklab(0.72 -0.13 0.075 / 0.3)` | rgb(35,72,50) | 1.796 |
| awaiting | `oklab(0.76 0.026 0.148 / 0.3)` | rgb(78,64,26) | 1.820 |

Fix 1 전 idle 은 `--ink-faint` 불투명(rgb(91,101,104), 대비 3.08)이라 형제보다
**밝게 튀었다**. 지금은 셋 중 가장 어두워 "상태 틴트가 아니다"라는 의도와 맞다.

### Fix 2 / 커버리지 매트릭스 3상태 — **결함 발견, 수정함**

WebKit 이 실제로 래스터한 값:

| 상태 | CSS | 실제 sRGB |
|---|---|---|
| covered | `oklch(0.72 0.15 150)` | rgb(83,190,112) |
| expected | `oklch(0.19 0.012 215)` (`--surface-raised`) | rgb(14,21,23) |
| none | `transparent` → body `oklch(0.13 0.012 215)` | rgb(3,8,10) |
| (격자선) | `oklch(0.27 0.014 215)` (`--border-hairline`) | rgb(31,40,43) |

대비비: covered/expected **7.878**, covered/none **8.598**,
**expected/none 1.091**.

즉 **expected 와 none 이 구분되지 않았다.** OKLCH 상으로는 19% 대 13% 로 6pp
차이지만, 이 저명도 구간에서 sRGB 인코딩이 압축되어 rgb(14,21,23) 대
rgb(3,8,10) 로 붕괴한다. 커버리지 매트릭스의 핵심 어포던스인 3상태 중 둘을
사용자가 분간할 수 없었다.

지난 런이 이걸 못 잡은 이유는 두 가지다 — (a) mock 시나리오의 모든 task 가
`req_ids: REQ_IDS`(5개 전부)를 기대해 `none` 셀 인스턴스가 아예 없었고
(`src/lib/mock-source.ts:98`), (b) 스크린샷 육안 비교로는 1.09 대비를 판별할 수
없다.

**명도 사다리로는 해결되지 않는다** — 같은 축에서 올릴 수 있는 최대치인
`--surface-overlay`(22%)로 바꿔도 1.165, `--border-hairline`(27%)까지 올려도
1.339 다. 기존 토큰 중 3:1 을 넘기는 것은 `--ink-faint`(50%, 3.362)뿐인데,
그건 중간 회색 블록이 격자를 가득 채워 뷰의 시각 무게를 크게 바꾼다.

**채택한 수정**: 채움색 대신 **윤곽의 유무**로 구분한다.

```css
.req-matrix__cell--none {
  background: transparent;
  border-color: var(--surface-panel);
}
```

none 셀의 테두리가 배경 대비 1.040 이 되어 격자에서 빠지고, expected 셀은
1.227 윤곽을 유지한다. "기대되지 않는 칸은 빈 공간, 기대되는 칸은 격자"로
읽힌다. 새 토큰을 만들지 않았고 다크 절제된 톤도 유지된다.

회귀 방지: `src/features/artifacts/index.test.tsx` 에 covered/expected/none 세
클래스가 각각 렌더되는지 못 박는 테스트를 추가했다(클래스를 하나로 합치면
실제로 실패하는 것을 확인).

### Fix 3 / `.layout__right` 50/50 분할 — 통과, 여러 높이에서 확인

`grid-template-rows: minmax(0, 1fr) minmax(0, 1fr)` 가 이 엔진에서도 의도대로
동작한다. 컨테이너 높이를 바꿔가며 두 패널의 `getBoundingClientRect().height`
를 측정했다:

| 컨테이너 높이 | thread | roster | roster 내부 스크롤 |
|---|---|---|---|
| 300 | 150 | 150 | 예 |
| 400 | 200 | 200 | 예 |
| 500 | 250 | 250 | 예 |
| 600 | 300 | 300 | 예 |
| 700 | 350 | 350 | 예 |
| 793 | 396.5 | 396.5 | 예 |
| 1000 | 500 | 500 | 예 |
| 자연 상태 (661) | 330.5 | 330.5 | 예 |

전 구간에서 정확히 50/50 이고, 넘치는 쪽은 각자 `overflow-y: auto` 안에서
스크롤된다. 300px 까지 줄여도 깨지지 않는다.

## 4. 콘솔

전 측정 통틀어 페이지 에러 0건(`window.onerror` / `unhandledrejection` /
`console.error` 후킹). 위 "수집기 생존 확인"대로 대조 로그가 매번 잡혔으므로
이 0건은 유효하다.

## 확인되지 않은 것 (범위 밖)

- **Windows WebView2** — macOS WKWebView 만 측정했다.
- **실제 런이 진행된 상태의 화면** — Tauri 안에서는 실제 Rust 백엔드가 붙는데
  이 환경에서 task 가 idle 을 벗어나지 않았다. spec/dag 는 정상 수신됐다
  (매트릭스 헤더에 `t-pm … t-qa` 5개 task 렌더). 배지·셀 상태는 클래스 강제로
  측정했다.
- **스크린샷** — macOS 화면 녹화 권한 부재로 캡처하지 못했다. 색 판정은 엔진이
  래스터한 sRGB 값으로 대체했다(자체 OKLCH→sRGB 변환기가 엔진 측정값 4종과
  정확히 일치함을 대조 확인).

## 5. t1-live-events 후속 조사 — "배지가 idle을 벗어나지 않는다"는 결함이 아니다

측정 날짜: 2026-09-02 (§3의 후속). 위 §3/확인되지 않은 것에서 관찰된 "task가
idle을 벗어나지 않았다"는 실제 `npm run tauri dev`(WKWebView, 이 문서 상단의
동일 엔진)로 재현·계측한 결과 **코드 결함이 아니다** — 파이프라인 전 구간이
정상 동작하며, 스크립티드 데모가 사람 눈으로 포착하기 어려울 만큼 빨리
끝나는 것이 원인이다.

**측정 방법**: §1의 프로브 방법(WKWebView는 CDP가 없어 `index.html`에 한 줄
주입 + `src/__verify__/probe.ts` + `127.0.0.1:1421` 수집 서버)을 그대로
썼다. 두 단계로 계측했다 — 먼저 `invoke("start_run")`과 별도 `listen("run://
event")`를 직접 호출해 Rust→webview 브리지 자체를 검증했고, 다음으로 실제
프로덕션 경로(`useRunStore.getState().startRun(goal)` → `TauriEventSource`
→ zustand store)를 그대로 호출해 UI가 실제로 보는 상태를 검증했다. **검증
후 두 주입 모두 되돌렸고 저장소에는 남아 있지 않다** (`git status`로 확인).

**1단계 — Rust emit은 한 번도 실패하지 않았다.** `lib.rs`의 `let _ = app.emit(...)`
를 `if let Err(err) = app.emit(...) { tracing::warn!(...) }`로 바꾼 뒤(현재
소스에 반영된 수정, 아래 "수정" 절 참조) 진단용으로 성공/실패를 모두
`eprintln!`(임시, 되돌림)으로도 찍어 확인했다. 한 런(`RunStarted`부터
`RunFinished`까지, `SprintFinished`/`RosterChanged` 포함 총 84건)에서
**emit 실패 0건** — `TaskStateChanged` 10건, `Message` 18건 전부 `Ok(())`.

**2단계 — webview `listen`도 전부 수신한다.** `invoke("start_run")`과 별도로
등록한 `listen("run://event", ...)`가 같은 84건을 전부 받았다(카운트가
Rust 쪽 emit 횟수와 정확히 일치). 브리지(§ 의심 구간이었던 `lib.rs:31` ↔
`tauri-source.ts:98`) 자체는 결함이 없다.

**3단계 — store도 정확히 반영한다.** 실제 프로덕션 경로(`useRunStore.startRun`)
로 다시 실행해 스토어 상태를 직접 읽었다:

| 시점 | `taskStates` | `messages.length` | `finished` |
|---|---|---|---|
| `startRun()` 프라미스 resolve 직후 (+0.23s) | `{"t-pm":"assigned"}` | 0 | `null` |
| +2.4s 하트비트 | 5개 전부 `"accepted"` | 18 | `"completed"` |

`t-pm: "assigned"`는 `deriveRail`(`src/features/rail/derive.ts:91`)이
working/awaiting 배지로 매핑하는 바로 그 상태다 — **badge 트리거 상태가
실제로 발생함을 직접 관찰했다.** 단, `roleCard`(같은 파일 75-97행)는 `state
!== "assigned"`이면 명시적으로 `idle`을 반환한다 — task가 `accepted`(완료)로
넘어가면 배지가 의도적으로 idle로 돌아간다(계약 §C5: "작업 중"이 아니면
idle). 스크립티드 데모가 계획된 rework 1회를 포함해 전체 5-role 스프린트를
**약 0.2~2.4초 안에 끝내므로**, "working" 구간은 실존하지만 매우 짧다 —
화면을 몇 초 늦게 보거나 스크린샷 한 장으로 판단하면 "idle밖에 없었다"로
보이기 쉽다. §3에서 "task가 idle을 벗어나지 않았다"고 적은 것은 바로 이
타이밍 때문으로 보인다(그 세션은 CSS 판정이 목적이라 클래스를 강제했고,
자연 발생을 기다리지 않았다).

**결론 (plan D8)**: `app.emit` → `listen` → zustand store → `deriveRail`
전 구간에 결함이 없다. "결함"이 아니라 "의도된 동작(스크립티드 데모가
사람이 놓치기 쉬울 만큼 빠르다)"으로 판정한다. t1이 유일하게 남긴 실질
수정은 `lib.rs`의 `let _ = app.emit(...)` → `tracing::warn!` 로깅 전환
(원인 규명 여부와 무관하게 필요했던 관측성 개선)과, `src-tauri/src/core.rs`
의 회귀 테스트(`scripted_run_pumps_task_state_changed_and_message_after_
spec_ready`) — SpecReady 이후 TaskStateChanged/Message가 실제로 펌프까지
나온다는, 이전에는 어떤 테스트도 증명한 적 없던 사실을 고정한다.

**t2에 대한 함의**: t2(실런 시각 검증)가 "라이브 이벤트가 도달하면"이라는
전제로 계획됐다면 그 전제 자체는 참이다(도달한다) — 다만 "working/awaiting
배지가 화면에 몇 초 이상 유지된다"는 가정이 있었다면 이 절의 타이밍
데이터로 재점검이 필요하다.
