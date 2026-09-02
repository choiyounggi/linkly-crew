# crew-app 폰트 번들 예산

측정 날짜: 2026-09-01
베이스 커밋: `ff6eb3718527c352db400abe63c89344fb72dde4` (이 문서를 만든 워크트리의
작업은 이 커밋 위에 아직 커밋되지 않은 변경으로 존재한다 — `git status --porcelain`
기준 `src/index.css` 1개 파일)
측정 환경: macOS(Darwin 25.1.0), Node v26.7.0, npm 11.19.0, `apps/crew-app`
디렉토리, `npm ci` 로 fresh install 후 측정.

이 앱은 Tauri 2 데스크톱 앱이다 — 폰트는 네트워크가 아니라 로컬 파일시스템/앱
번들에서 로드된다. 그래서 이 문서가 줄이려는 것은 **네트워크 지연이나 LCP 가
아니라 배포 산출물 크기**(설치본·디스크 사용량)다. 이 구분이 아래 서브셋팅
제안의 권고 방향에 직접 영향을 준다.

## Before/After

측정 명령(공통: `cd apps/crew-app && rm -rf dist && npm run build` 후 아래 각
명령 실행). Before 는 fontsource `@import` 9줄 상태(이 태스크 착수 전), After
는 이 태스크가 만든 손으로 쓴 `@font-face` 9개(woff2 만 참조) 상태다.

| 항목 | 명령 | Before (bytes) | Before (KB) | After (bytes) | After (KB) | 증감 |
|---|---|---|---|---|---|---|
| 폰트 총합 (.woff+.woff2) | `find dist/assets -type f \( -name '*.woff' -o -name '*.woff2' \) -exec stat -f%z {} + \| awk '{s+=$1} END {print s}'` | 4,460,496 | 4356.0 | 1,639,240 | 1601.0 | **-2,821,256 bytes (-2755.1KB, -63.3%)** |
| .woff2 합계 | `find dist/assets -type f -name '*.woff2' -exec stat -f%z {} + \| awk '{s+=$1} END {print s}'` | 1,639,240 | 1601.0 | 1,639,240 | 1601.0 | 0 (그대로 — woff2 는 건드리지 않음) |
| .woff 합계 | `find dist/assets -type f -name '*.woff' -exec stat -f%z {} + \| awk '{s+=$1} END {print s}'` | 2,821,256 | 2755.1 | 0 | 0 | **-2,821,256 bytes (-100%)** |
| 한글 woff2 합계 (`sans-kr`) | `find dist/assets -type f -name '*sans-kr*.woff2' -exec stat -f%z {} + \| awk '{s+=$1} END {print s}'` | 1,525,168 | 1489.4 | 1,525,168 | 1489.4 | 0 |
| JS 합계 | `find dist/assets -type f -name '*.js' -exec stat -f%z {} + \| awk '{s+=$1} END {print s}'` | 423,261 | 413.3 | 423,261 | 413.3 | 0 |
| CSS 합계 | `find dist/assets -type f -name '*.css' -exec stat -f%z {} + \| awk '{s+=$1} END {print s}'` | 43,876 | 42.85 | 43,207 | 42.19 | -669 bytes (-0.66KB) |
| .woff 파일 개수 | `find dist/assets -name '*.woff' \| wc -l` | 9 | — | 0 | — | -9 |
| .woff2 파일 개수 | `find dist/assets -name '*.woff2' \| wc -l` | 9 | — | 9 | — | 0 |

원본 측정치와 재현 절차는 `.orchestration/notes/t2-baseline.md`(before) 와
`.orchestration/notes/t2-woff-removal.md`(after)에 있다. 이 표의 숫자는 그 두
문서에서 그대로 옮긴 것이 아니라 — 두 문서 각각이 `npm ci` 후 fresh build 에서
직접 측정한 결과다(같은 명령을 두 번 다른 시점에 실행).

## `.woff` 제거 — 무엇을 했고 왜 이 방식인가

`@fontsource/*` 의 CSS 는 한 `src:` 선언 안에 `format('woff2')` 와
`format('woff')` 를 함께 참조한다(`node_modules/@fontsource/ibm-plex-sans-kr/
korean-400.css` 확인). Vite 는 CSS 의 `url()` 을 전부 애셋으로 emit 하므로
`.woff` 도 같이 번들됐다. `.orchestration/notes/t2-baseline.md` 에서 확인한 대로
런타임에 `.woff` 요청은 0건이었다(Chromium 계열 엔진, `performance.
getEntriesByType('resource')` 로 확인, 같은 호출에서 `.woff2` 요청 8건은 잡혀
수집기가 살아있음을 대조 확인).

**1순위(인라인 PostCSS 플러그인으로 `format('woff')` 만 제거)는 실패했다.**
`vite.config.ts` 에 `@font-face` 의 `src:` 에서 `format('woff')` 세그먼트를
지우는 PostCSS 플러그인을 추가했더니, 빌드된 CSS **텍스트**에서는 `.woff`
참조가 사라졌지만 `dist/assets/` 의 `.woff` **파일**은 9개 그대로 남았다. Vite
의 CSS 애셋-URL 리라이트가 우리 플러그인보다 먼저 실행돼 원본 `src:` 를 읽고
`.woff` 를 이미 emit 대상으로 등록해버리기 때문으로 관찰됐다(자세한 재현은
`.orchestration/notes/t2-woff-removal.md`). 이 방식은 이 Vite 버전(6.4.3)에서
판정 게이트(`dist/assets` 의 `.woff` 개수 0)를 통과하지 못해 폐기했다.

**2순위(손으로 쓴 `@font-face`)를 채택했다.** `src/index.css` 의 `@import
"@fontsource/.../latin-*.css"` / `korean-*.css` 9줄을, 각 weight(300/400/700) ×
family(mono/sans/sans-kr) 조합마다 `.woff2` 만 참조하는 `@font-face` 블록
9개로 대체했다. `font-family` 이름은 fontsource 원본과 동일하게 유지해
`src/styles/tokens.css` 의 `--font-sans`/`--font-mono` 참조가 그대로 맞는다.

**렌더 확인**: `getComputedStyle(...).fontFamily` 는 폴백이 일어나도 선언값을
그대로 보고하므로 쓰지 않았다. 대신 `document.fonts.check()` (실제 로드된
페이스 판정)로 mono/sans/sans-kr 각 weight 조합이 전부 로드됨을 확인했고,
`document.body.innerText` 로 실제 렌더된 한글 텍스트(보드/로스터 등 다수 화면)를
확인했다 — 상세는 `.orchestration/notes/t2-woff-removal.md`.

## 서브셋팅 제안 (구현하지 않음 — 제안만)

### 예상 절감

정적 UI 라벨(코드에 하드코딩된 한글 문자열)만 커버하는 서브셋을 만들면 고유
음절 257자만 포함하면 된다. 추출 방법: `src/**/*.ts(x)` 중 `*.test.*` 와
`src/test/` 를 제외한 27개 파일을 읽어 `[가-힣]` 에 매치되는 문자를 `Set` 으로
중복 제거한다(2026-09-02 재실행에서 지난 런과 동일하게 27파일/257자 재현).

**2026-09-02 실측** (fonttools 4.64.0 `pyftsubset`, `--flavor=woff2
--layout-features='*'`, 입력은 `node_modules/@fontsource/ibm-plex-sans-kr/files`
원본):

| 파일 | 원본 (bytes) | 257자 서브셋 (bytes) | 비율 |
|---|---|---|---|
| `ibm-plex-sans-kr-korean-300-normal.woff2` | 510,300 | 26,944 | 5.28% |
| `ibm-plex-sans-kr-korean-400-normal.woff2` | 517,652 | 27,380 | 5.29% |
| `ibm-plex-sans-kr-korean-700-normal.woff2` | 497,216 | 30,280 | 6.09% |
| **한글 3개 합계** | **1,525,168** | **84,604** | **5.55%** |

절감은 1,440,564 bytes(1,406.8KB, -94.45%)다. 폰트 총합은 1,639,240 →
198,676 bytes 가 된다.

이 문서의 이전 판이 세운 "woff2 파일 크기는 글리프 수에 거의 선형 비례한다"는
가정은 **빗나갔다** — 글리프 수는 11,172자 대비 2.3% 인데 바이트는 5.55% 다
(woff2 의 테이블·힌팅 등 글리프 수에 비례하지 않는 고정 오버헤드 때문).
절감 방향의 결론(자릿수 단위)은 맞았지만 배율 근거는 틀렸으므로, 이후 판단은
위 실측표를 근거로 할 것.

### 관찰된 위험

`.orchestration/notes/t2-subset-risk.md` §2, §3 에 근거:

1. **정적 257자 서브셋으로는 부족하다.** 런타임에 한글이 생성되는 경로가
   최소 4곳 확인됐다 — 사용자 goal 입력(`src/App.tsx:27,43-49,64-68`), 로스터
   모델 필드 자유 입력(`src/features/roster/index.tsx:201`), 파싱된 에이전트
   메시지 본문(`src/features/thread/body.ts` `Artifact.content`,
   `ChangeRequestBody.reason`), 파싱 안 된 메시지의 raw JSON 폴백
   (`src/features/thread/index.tsx:64`, 여러 `MessageKind` 가 여기로 떨어짐).
   이 네 경로 모두 LLM 에이전트 또는 사용자가 자유롭게 채우는 텍스트라 257자
   서브셋 밖 글자가 실제로 나타난다.
2. **서브셋 밖 글자는 `system-ui` 로 완전히 폴백된다(같은 family 의 다른
   weight 로 대체되는 게 아니다).** `unicode-range: U+AC00` 로 300-weight 얼굴을
   제한하는 실험에서, 캔버스 `measureText` 로 서브셋 밖 글자('시')의 렌더
   폭이 `IBM Plex Sans KR` weight 400/700 폭과는 다르고 명시적 `system-ui`
   요청 폭과 소수점까지 정확히 일치함을 확인했다(`t2-subset-risk.md` §3-b).
3. **하지만 이 macOS/Chromium 조합에서는 그 폴백이 육안으로 거의 안 보였다.**
   서브에이전트에 위임한 스크린샷 비교에서 narrowed 상태와 reverted 상태 사이에
   tofu, 자형 붕괴, 눈에 띄는 weight 차이가 관찰되지 않았다(`t2-subset-risk.md`
   §3-a). 이는 "위험이 없다"는 뜻이 아니라, **이 특정 OS/엔진에서 우연히
   system-ui 의 한글 자형이 IBM Plex Sans KR 과 비슷해 보였다**는 뜻이다.
   Tauri 가 실제로 쓰는 WebKit(macOS)/WebView2(Windows) 에서, 또는 시스템에
   완성형 한글 폰트가 없는 환경에서 같은 결과가 나온다는 보장은 이 태스크의
   측정 범위 밖이다.

### 권고

**결정(2026-09-02, 사용자): 서브셋팅을 하지 않는다.** 위 실측으로 절감폭이
1,406.8KB(-94.45%)임이 확정됐지만, 아래 권고의 (b)(c) 위험이 그대로 남아 있고
Tauri 데스크톱 앱이라 지연 이득이 없다는 판단이 유지됐다. 절감 수치는 확정된
근거로 이 문서에 남기되 구현은 하지 않는다.

**서브셋팅을 지금 구현하지 않는 것을 권고한다.** 근거: (a) 이 앱은 Tauri
데스크톱 앱이라 폰트가 로컬에서 로드되므로 서브셋팅이 사 줄 이득은 네트워크
지연이 아니라 오직 설치본 디스크 크기이고, (b) 위 3번 관찰대로 폴백이
"눈에 띄지 않게" 실패할 수 있어 사용자가 문제를 알아채지 못한 채 잘못된 자형을
보게 될 위험이 있으며, (c) 런타임 생성 한글(에이전트 출력, 사용자 입력)의
범위를 코드가 원천적으로 제약할 수 없어 정적 분석만으로 안전한 서브셋 경계를
확정할 수 없다. 디스크 크기가 실제로 문제라면, 서브셋팅보다 먼저
`ibm-plex-sans-kr` 자체를 더 작은 한글 폰트로 교체하는 것(디자인 결정, 이
태스크 범위 밖)을 검토하는 편이 위험 대비 이득이 크다.

## Weight 드롭 제안 (구현하지 않음 — 제안만)

`.orchestration/notes/t2-subset-risk.md` §4 의 사용처 조사 결과:

| Weight | 토큰 | 사용처 | 드롭 시 영향 |
|---|---|---|---|
| 300 | `--font-weight-body` | 앱 전체 기본 본문 텍스트(`src/index.css:101`, `body` 전체) | 드롭하면 본문 전체가 400(regular)로 렌더된다 — 가장 광범위한 영향, 앱 전체 톤이 바뀐다 |
| 400 | `--font-weight-regular` | 공용 프리미티브 컴포넌트(`src/components/primitives.css:14,108`) | 이 프리미티브들이 폰트 파일 3개(mono/sans/sans-kr) 중 유일하게 남는 weight 가 되므로 드롭 후보에서 제외해야 한다 |
| 500 | `--font-weight-medium` | **없음**(코드에서 미참조, 정의만 존재) | 드롭해도 영향 없음 — 애초에 폰트 파일도 로드하지 않는다 |
| 600 | `--font-weight-semibold` | 아티팩트 패널, 보드 카드 제목, 검색 결과, 레일 라벨, 스레드 헤더 등 5곳 | 폰트 파일이 없어 이미 700(bold) 얼굴로 렌더되고 있다(캔버스 폭 측정으로 확인, `t2-subset-risk.md` §4) — 700 을 드롭하면 이 5곳도 함께 400 으로 밀린다 |
| 700 | `--font-weight-bold`/`--font-weight-heading` | 보드 강조, DAG 노드 강조, 레일 강조, 헤딩류 프리미티브(4곳) + 위 600 사용처 5곳(대체 렌더) | 드롭하면 직접 사용처 4곳뿐 아니라 600 을 요청하는 5곳도 함께 400 으로 밀려 총 9곳의 시각적 강조가 사라진다 |

**결정(2026-09-02, 사용자):**
- **700 드롭은 하지 않는다** — 직접 사용처 4곳에 더해 600 을 요청하는 5곳까지
  총 9곳의 강조가 400 으로 밀리기 때문.
- **`--font-weight-medium`(500)은 제거했다** — 코드 참조 0건(정의 1줄뿐)임을
  재확인하고 `src/styles/tokens.css` 에서 삭제. 폰트 파일을 로드하지 않던
  토큰이라 화면·번들 어느 쪽에도 영향이 없다.

## 확인되지 않은 것 (범위 밖)

- ~~Tauri 실제 webview(WebKit/WebView2)에서의 `.woff` 미로드 여부~~ —
  **2026-09-02 해소.** macOS Tauri webview(AppleWebKit/605.1.15, `isTauri()`
  = true)에서 `performance.getEntriesByType("resource")` 로 측정한 결과
  `.woff` 요청 0건 / `.woff2` 요청 9건(전 페이스)이었고, 9개 페이스 모두
  `document.fonts` 에서 `loaded` 상태이며 캔버스 렌더 폭이 `system-ui` 와
  달랐다. 상세는 `design/TAURI-WEBVIEW-VERIFY.md`.
- Windows WebView2 에서의 동일 검증 — macOS 만 측정했다.
- 다른 OS/폰트 환경에서 서브셋 폴백이 육안으로 눈에 띄는지 여부.
- 실제 서브셋 파일을 만들었을 때의 정확한 바이트 절감치(위 예상치는 글리프
  비율 기반 추정이며 실측이 아니다).
