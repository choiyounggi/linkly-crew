# M7 Spike Results — 가변 로스터 real-CLI 스팟체크

Real CLI: `claude`, run 2026-08-30, macOS (이 머신). 결과는 코디네이터가 수동으로 실행한
실제 테스트 출력 기준이며 추정이 아니다.

## Success criterion (M7 계약 Phase 0 ②)

> 수동 검증 = GUI 1클릭 + 가변 로스터 real-CLI 스팟체크(함정 19).

**Result: PASS.** `m7_roster::real_cli_three_person_team_completes_one_sprint` —
3인팀(lead+developer+qa) 가변 로스터 구성으로 `RealCli` 하네스가 1스프린트를 사람 개입
없이 완주. **elapsed = 78.87s.**

## 실측 상세

- 테스트: `m7_roster::real_cli_three_person_team_completes_one_sprint`
- 로스터: lead + developer + qa (3인, 가변 로스터 — 정식 5역할 중 부분집합)
- 하네스: `RealCli` (실제 `claude` 프로세스, 스크립트/목 아님)
- 결과: 통과, **78.87s**, 2026-08-30
- 실행 주체: 코디네이터 수동 (전 워커 `--ignored` 실행 금지, 계약 E0)

## Findings

### 1. `cargo test --workspace` 콜드 빌드 직후 간헐 플레이크 (원인 미상)

콜드 빌드 직후 `cargo test --workspace` 첫 실행에서만 2회 실패 목격:
- 1차: `m7_roster::three_person_team_run_completes_...` — 4회 런 중 1회 실패.
- 2차: 미상 3-테스트 바이너리 1건 — `2 passed; 1 failed; 0.10s`.

직후 재실행은 각각 3+·6연속 clean, 격리 실행 8/8 clean, 실패 메시지도 미포착. 원인
미상(병렬 부하/콜드 스타트 타이밍 의심). `HANDOFF.md` §5 함정 20 참고.

### 2. 오케스트레이션 운영 — 워커가 `impl_done` 후 커밋 전 사망

m7a 재진입 시 4개 태스크(t-artifacts/t-inbox/t-rosterrun/t-search)가 `impl_done` 보고
후 커밋 전 워커 사망 — 구현이 워크트리 dirty로만 존재했고 코디네이터가 스냅샷 커밋으로
회수했다. merge-prep 프롬프트(커밋 지시) 전달 전 워커 생존 확인 필요. `HANDOFF.md` §5
함정 21 참고.

## M7 계약 정본

`.orchestration/contracts-m7.md` — teardown 시 `archive-20260830-m7a/contracts-m7.md`로
아카이브 예정 (M6 표기 관례와 동일).
