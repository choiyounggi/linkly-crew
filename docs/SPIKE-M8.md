# M8 Spike Results — pi RPC 스파이크 + real-CLI 스팟체크

원시 캡처: `~/.linkly-crew/pi-spike/rpc2.jsonl`. RPC 스파이크·real-CLI 스팟체크 모두
코디네이터가 이 맥북에서 수동 실행한 실측 결과이며 추정이 아니다.

## Success criterion (M8 계약 Phase 0 ③)

> 검증 = 결정론 fake + real-CLI `#[ignore]` 스팟체크(코디네이터 수동, 함정 19).

**Result: PASS.** `crew-harness tests/real_pi.rs one_turn_round_trip` — 실제 `pi`
프로세스로 1턴 라운드트립을 완주. **elapsed = 6.70s.**

## 실측 상세 — real-CLI 스팟체크

- 테스트: `crew-harness tests/real_pi.rs::one_turn_round_trip`
- 하네스: PiHarness (실제 `pi` 프로세스, 스크립트/목 아님)
- 결과: 통과, **6.70s**, 2026-08-31
- 실행 주체: 코디네이터 수동 (전 워커 `--ignored` 실행 금지, 계약 F0)

## RPC 스파이크 실측 (원시: `~/.linkly-crew/pi-spike/rpc2.jsonl`)

`pi` v0.75.5 (`/opt/homebrew/bin/pi`, @earendil-works/pi-coding-agent), RPC 1턴 스파이크
성공(gpt-5.5 응답 "OK").

### 1. stdin EOF 시 즉시 셧다운

`pi --mode rpc`는 stdin EOF를 받으면 **턴 완료 전이라도 즉시 셧다운**한다. 어댑터는
세션 수명 동안 stdin 파이프를 열어둬야 한다. (→ `Session.stdin`을
`Arc<tokio::sync::Mutex<ChildStdin>>`으로 보정한 근거, F3. `HANDOFF.md` §5 함정 22.)

### 2. `agent_settled`는 종결 판정에 쓸 수 없다

정상 턴 순서: user `message_end` → assistant `message_start/update/end`
(`stopReason:"stop"`, `model` 필드 포함) → `turn_end`(message+toolResults) →
`agent_end`(`willRetry:false`). 문서상의 `agent_settled` 이벤트는 **90초 내 미발화** —
종결 판정에 사용 금지, `agent_end`(willRetry:false)를 턴 종결 백스톱으로 삼는다.
(`HANDOFF.md` §5 함정 23.)

### 3. `extension_ui_request` 스팸 + 다이얼로그 스톨 위험

1턴에 `extension_ui_request`가 89건 발생(setWidget/setStatus/setTitle 등 무응답형
스팸). 다이얼로그형(method: select/confirm/input/editor)은 응답이 없으면 스톨 위험 —
PiHarness는 즉시 `{"type":"extension_ui_response","id":<id>,"cancelled":true}`로
자동 응답한다. (`HANDOFF.md` §5 함정 24.)

### RPC 송신 형식 (공식 rpc.md)

`{"type":"prompt","message":"..."}` / `{"type":"set_model","provider":"..","modelId":".."}`
/ `{"type":"abort"}` — 1줄 1 JSON. 실패 신호: assistant `stopReason:"error"`,
`auto_retry_end`의 `success:false`+`finalError`, 응답 객체 `success:false`+`error`.

## M8 계약 정본

`.orchestration/contracts-m8.md` — teardown 시 `archive-20260831-m8a/contracts-m8.md`로
아카이브 예정 (M7 표기 관례와 동일).
