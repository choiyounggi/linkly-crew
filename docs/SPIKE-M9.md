# SPIKE-M9 — 스폰 세션 훅 차단 실측 (함정 8)

**측정자**: 코디네이터(수동, 함정 19 — 실 CLI 실행은 워커 금지)
**일시**: 2026-08-31
**환경**: macOS Darwin 25.1.0, `claude` **2.1.236**, `ANTHROPIC_API_KEY` **미설정**(구독 OAuth)
**원 문제**: 함정 8 — cwd 격리는 유저 전역 훅을 막지 못한다. 스폰된 claude 세션에서
유저 전역 훅이 발화해 계약 JSON 뒤에 프로즈가 붙고 `extract_json`이 실패 → Blocked →
에스컬레이션 → E2E 타임아웃 (M3 실측, 2026-08-27).

---

## 1. 후보 레버 조사 (`claude --help`, v2.1.236)

| 플래그 | 훅 차단 | 구독 OAuth 유지 | 판정 |
|---|---|---|---|
| `--bare` | ○ ("skip hooks, LSP, plugin sync…") | **×** — "Anthropic auth is strictly ANTHROPIC_API_KEY or apiKeyHelper via `--settings` (OAuth and keychain are **never** read)" | **사용 불가**. 이 프로젝트의 전제(구독 CLI)를 파괴 |
| `--setting-sources <user,project,local>` | 검증 대상 | 검증 대상 | **채택 후보** |

`--bare`는 문서상 훅을 막지만 OAuth·키체인을 절대 읽지 않으므로, "구독 중인 AI CLI를
팀원으로 묶는다"는 프로젝트 전제와 정면 충돌한다 — 실측 없이 배제.

---

## 2. 실측 — 3개 런

프롬프트는 3런 모두 동일: `Reply with exactly this JSON and nothing else: {"ok":true}`

| 런 | 인자 | cwd |
|---|---|---|
| A (베이스라인) | `-p --output-format stream-json --verbose` | 빈 디렉토리 |
| B (격리) | A + `--setting-sources project,local` | 빈 디렉토리 |
| C (프로덕션 형상) | `-p --input-format stream-json --output-format stream-json --verbose --session-id <uuid> --setting-sources project,local`, stdin으로 stream-json 유저 메시지 주입 | 빈 디렉토리 |

C는 `ClaudeCodeHarness::spawn`(`crates/crew-harness/src/claude.rs:138-153`)의 인자
구성을 그대로 재현한 것 — 플래그가 실 스폰 경로와 조합되는지 확인이 목적.

### 결과

| 지표 | A (베이스라인) | B (격리) | C (프로덕션 형상) |
|---|---|---|---|
| `hook_started` 이벤트 | **9** (전부 `SessionStart:startup`) | **0** | **0** |
| `result.subtype` | `success` | `success` | `success` |
| `result.is_error` | `false` | `false` | `false` |
| `apiKeySource` | `none` (= 구독 OAuth) | **`none`** | — |
| 프로세스 exit code | 0 | 0 | 0 |
| `plugins` | 18 | **0** | — |
| `mcp_servers` | 7 | **1** (`github`) | — |
| `skills` | 92 | **16** | — |
| `agents` | 16 | **5** | — |
| `slash_commands` | 136 | **48** | — |
| `cache_creation_input_tokens` | **24,667** | **9,998** | — |

---

## 3. 결론

1. **`--setting-sources project,local`은 유저 전역 훅을 완전히 차단한다.**
   `hook_started` 9건 → 0건. 유저 전역 `~/.claude/settings.json`의
   `SessionStart`/`Stop`/`PreToolUse` 등 12개 이벤트 훅이 전부 로드 대상에서 빠진다.

2. **구독 OAuth 인증은 유지된다.** `ANTHROPIC_API_KEY`가 미설정인 상태에서
   `apiKeySource: "none"`으로 정상 완주 — 설정 소스와 인증 경로가 분리되어 있음을
   실측으로 확인. `--bare`와 결정적으로 다른 지점이며, 이것이 `--bare`를 배제하고
   `--setting-sources`를 채택하는 근거다.

3. **실 스폰 형상과 조합된다.** `--input-format stream-json` + `--session-id` 조합에서도
   훅 0건 · `is_error:false`.

4. **부수 효과 — 프리앰블 ~60% 감소** (24,667 → 9,998 토큰, -14,669).
   플러그인 18→0, 스킬 92→16, MCP 7→1이 그대로 프리앰블에서 빠진다. 함정 2
   ("호출당 프리앰블 40~52k")에 대한 직접적 완화이기도 하다.

---

## 4. 한계 — 결론에 넣지 말 것

- **M3 실패의 재현이 아니다.** A 런에서도 프로즈 오염은 관측되지 않았다(단일 턴,
  자명한 프롬프트). 이 스파이크가 증명한 것은 *메커니즘*(훅이 로드되는가/안 되는가)이지
  M3 E2E 실패의 재현이 아니다. 훅이 0건이면 훅발 프로즈도 0건이라는 것은 연역이다.
- **`extract_json` 균형 중괄호 폴백을 제거하지 말 것.** 훅 차단은 훅발 오염만 막는다.
  모델 자체가 JSON 앞뒤에 설명을 붙이는 경우는 여전히 폴백이 유일한 방어선이다.
- **auto-memory는 여전히 로드된다.** `memory_paths.auto`가 A/B 양쪽 동일
  (`~/.claude/projects/<mapped>/memory/`). 설정 소스와 무관한 경로이므로
  `--setting-sources`로 막히지 않는다. 훅은 아니지만 스폰 세션에 외부 텍스트가
  주입되는 잔여 경로다 — 별건으로 남긴다.
- **프로젝트 훅은 계속 로드된다.** `project,local`을 남긴 선택의 귀결이다. 스폰
  대상 워크스페이스가 자체 훅을 두면 그건 그대로 발화한다(의도된 동작 — 유저 개인
  환경만 차단하는 것이 목적).
- **버전 의존.** v2.1.236 실측이다. `--setting-sources`의 의미가 바뀌면 재측정 필요.

## 5. 재현 방법

```sh
claude -p --output-format stream-json --verbose 'Reply with exactly this JSON and nothing else: {"ok":true}' > a.json
claude -p --output-format stream-json --verbose --setting-sources project,local 'Reply with exactly this JSON and nothing else: {"ok":true}' > b.json
jq -s '[.[] | select(.type=="system" and .subtype=="hook_started")] | length' a.json   # 9
jq -s '[.[] | select(.type=="system" and .subtype=="hook_started")] | length' b.json   # 0
jq -c 'select(.subtype=="init") | {apiKeySource, plugins:(.plugins|length), skills:(.skills|length)}' a.json b.json
```
