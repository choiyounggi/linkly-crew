# t1-foundation — 디자인 방향 결정

캔버스 아티팩트: https://claude.ai/code/artifact/97138eca-f0e5-47f2-9a19-3fc1e981f4f0

3개 방향 아트보드가 발행되어 있다. 서로 다른 축을 탐색하며, 색만 다른 변주가 아니다.

## 방향 1 — 터미널 밀도

- **축**: 밀도 (density) — 터미널처럼 빽빽함
- **앵커 hue**: 약 215° (cool blue-cyan)
- **폰트 쌍**: heading IBM Plex Mono (700) / body IBM Plex Sans (400)
- **동기**: 병렬 에이전트 다수를 한 화면에서 스캔해야 하는 오케스트레이션 앱 특성상,
  밀도가 정보 처리량을 좌우한다.
- **트레이드오프**: 초심자에게 빽빽해 보이고 터치 타깃이 빡빡해질 위험.

## 방향 2 — 레이어드 서피스

- **축**: 표면 (surface) — 명도 계단으로 뚜렷한 층 (paper → panel → raised → overlay)
- **앵커 hue**: 약 55° (warm amber)
- **폰트 쌍**: heading/body Public Sans (700/400), 숫자 JetBrains Mono
- **동기**: 장시간 세션의 시각 피로를 줄이고, 승인함·스레드처럼 중첩된 패널의 층위를
  명도만으로 뚜렷이 구분한다.
- **트레이드오프**: 정보 밀도가 낮아져 스크롤이 늘어난다.

## 방향 3 — 모노 중심 기술적

- **축**: 타이포 (typography) — heading·body 모두 모노스페이스
- **앵커 hue**: 약 280° (violet)
- **폰트 쌍**: heading/body 모두 JetBrains Mono (700/300)
- **동기**: 코드/에이전트 로그와 UI 텍스트가 시각적으로 통일되어 개발자 도구라는
  정체성이 강해진다.
- **트레이드오프**: 긴 한국어 문장에서는 가변폭 대비 가독성이 다소 떨어진다.

## 선택

**방향 1 — 터미널 밀도** (밀도축, 앵커 hue 약 215° cool blue, heading IBM Plex Mono 700 / body IBM Plex Sans 400).

### 한글 타이포그래피 추가 결정 (D7 확장)

라틴 폰트만으로는 한글 라벨(보드·DAG·타임라인·승인함·아티팩트)이 시스템 폰트로
폴백된다. 한글 동반 페이스를 명시적으로 짝짓는다:

- `--font-sans`: `"IBM Plex Sans", "IBM Plex Sans KR", system-ui, sans-serif`
- `--font-mono`: `"IBM Plex Mono", "IBM Plex Sans KR", ui-monospace, monospace`
  (모노 문맥의 한국어는 Plex Sans KR 로 떨어져 설계 언어를 유지한다)

이 앱은 Tauri 데스크톱 앱이라 CDN 을 가정할 수 없다 — 폰트는 로컬 번들
(`@fontsource/*`)로 제공한다.
