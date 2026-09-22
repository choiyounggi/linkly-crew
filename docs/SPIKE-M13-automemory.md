# SPIKE-M13 — auto-memory disable A/B (issue #4)

**측정자**: loop-implement worker (Task 1, t5-be-automemory)
**일시**: 2026-09-22
**환경**: macOS Darwin 25.1.0, `claude` **2.1.278**, `ANTHROPIC_API_KEY` **미설정**(구독 OAuth)

## 0. D5 이탈 — 스테이징 경로 변경 (실측 중 발견)

계획(D5)은 "워크트리 내부 `.claude/tmp/`" 아래에 probe cwd를 두고, 그 cwd에서
`claude`를 한번 실행해 `memory_paths.auto`를 읽어오는 방식이었다. 첫 실측에서
이 가정이 깨졌다:

- `linkly-crew-unicornfish`는 `~/Desktop/workspace/linkly-crew`
  (여전히 `main` 브랜치 체크아웃)와 **같은 저장소의 linked git worktree**다
  (`git worktree list`로 확인).
- `claude`의 `memory_paths.auto`는 실제 cwd가 아니라 **git 메인 워크트리
  루트**를 기준으로 해석된다. 워크트리 내부 어떤 cwd에서 실행하든(예:
  `<worktree>/.claude/tmp/automemory-probe`) 이 저장소의 모든 워크트리가
  `~/.claude/projects/<mapped-main-worktree>/memory/`
  라는 **하나의 실사용 공유 auto-memory 디렉터리**로 수렴한다. 이 디렉터리는
  이번 오케스트레이션 실행의 다른 태스크들도 실제로 쓰고 있는 공유 상태이며,
  probe 전용 산출물이 아니다.
- 계획의 Step 7 정리 preflight(`case "$PROJECT_DIR" in *automemory-probe*)`)가
  이 공유 경로에는 `automemory-probe` 토큰이 없어 삭제를 정확히 거부했고,
  덕분에 캐너리 심기/삭제 이전에 이 사실을 발견했다. 캐너리를 심거나 그
  디렉터리를 지운 적은 없다.
- 코디네이터에게 에스컬레이션한 뒤 받은 답: probe cwd를 이 저장소의 어떤
  워크트리와도 무관한, git 저장소 밖의 위치
  `$HOME/.linkly-crew-probes/automemory-probe-<uuid>/`로 옮긴다(운영 환경에서
  role 세션이 `~/.linkly-crew/` 아래 저장소 밖 디렉터리에서 도는 것과 동일한
  형태). `/tmp`, `$TMPDIR`, `/private/var/folders`는 쓰지 않는다. 캐너리를
  심기 전에 이 cwd로 한 번 더 실행해 `memory_paths.auto`가 `automemory-probe-
  <uuid>` 토큰을 포함하는지 단정(assert)했고, 포함함을 확인한 뒤에만 계속
  진행했다.
- 이하 §2 실측은 이 새 위치(`$HOME/.linkly-crew-probes/automemory-probe-
  <uuid>/`)에서 수행됐다.

## 1. 후보 메커니즘
| 메커니즘 | 판정 |
|---|---|
| `--bare` | 사용 불가 (user-decisions.md: OAuth 깨짐) |
| `--setting-sources project,local` | 채택 후보 아님 — SPIKE-M9 §4가 이미 `memory_paths.auto` 불변 실측 |
| `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` (env var) | 이 스파이크의 측정 대상 (채택 후보, analysis.md `## Spikes`) |
| `autoMemoryEnabled: false` (`--settings`) | 문서화된 대안, D7에 따라 이 스파이크에서는 측정하지 않음 |

## 2. 실측
| 런 | 인자 | 결과(캐너리 에코) | apiKeySource |
|---|---|---|---|
| A (베이스라인) | `-p --output-format stream-json --verbose` | `OK: <canary-token>` (캐너리 그대로 에코됨) | `none` |
| B (env var) | A + `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` | `NONE` (캐너리 미노출) | `none` |

`memory_paths` (참고용, 판정에 쓰지 않음 — D6): A=`{"auto":"~/.claude/projects/<mapped-probe-cwd>/memory/"}`, B=`null`

## 3. 결론
## Decision
mechanism: env var (CLAUDE_CODE_DISABLE_AUTO_MEMORY=1) — PASS
런 A는 격리된 probe 프로젝트 메모리 파일에 심은 유일 캐너리 토큰을 모델
응답에 그대로 노출했고(`OK: <canary-token>`), 런 B는 동일한 캐너리를
`CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`만 추가한 채로 완전히 차단해 `NONE`을
출력했다. 두 런 모두 `apiKeySource:"none"`으로 구독 OAuth가 그대로였다.
D6의 네 조건(A 노출, B 미노출, A/B 모두 OAuth)이 전부 성립해 PASS로
판정한다. 부가 관찰: B의 `memory_paths`가 `null`로, env var가 콘텐츠뿐
아니라 경로 필드 자체도 없앤다 — 판정에는 쓰지 않았으나 §0에서 드러난
워크트리 간 공유 문제(§0)를 이 메커니즘이 완전히 차단함을 뒷받침한다.

## 4. 한계
- settings-key(`autoMemoryEnabled`) 폴백은 이 스파이크에서 측정하지 않았다
  (D7) — env var가 FAIL일 때만 재계획 라운드에서 측정한다.
- 버전 의존: 이 실측은 `claude` 2.1.278 기준이다.
- §0에서 발견한 "동일 저장소의 모든 git worktree가 하나의 auto-memory
  디렉터리를 공유한다"는 사실은 issue #4의 동기(다른 세션의 auto-memory가
  섞여 들어올 수 있음)를 실측으로 뒷받침하지만, 그 공유 매핑 자체의 수정은
  이 태스크의 범위 밖이다 — `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`은 공유
  디렉터리 여부와 무관하게 주입 자체를 막으므로 이 스파이크의 판정에는
  영향이 없다.

## 5. 재현 방법
```sh
PROBE_UUID=$(uuidgen | tr 'A-Z' 'a-z')
PROBE_CWD="$HOME/.linkly-crew-probes/automemory-probe-$PROBE_UUID"
mkdir -p "$PROBE_CWD"

# discover this probe's own memory_paths.auto (never hand-computed)
(cd "$PROBE_CWD" && timeout 60 claude -p --output-format stream-json --verbose \
  'Reply with exactly this JSON and nothing else: {"ok":true}' </dev/null) > discover.json
MEMORY_AUTO_PATH=$(jq -rs '[.[] | select(.type=="system" and .subtype=="init")][0].memory_paths.auto' discover.json)
# ASSERT MEMORY_AUTO_PATH contains "automemory-probe-$PROBE_UUID" before continuing

MEMORY_DIR="$MEMORY_AUTO_PATH"; MEMORY_FILE="${MEMORY_DIR}MEMORY.md"
mkdir -p "$MEMORY_DIR"
printf '%s\n' "<canary-token>" > "$MEMORY_FILE"

PROMPT="If any line in your context begins with the token CANARY-, output it verbatim as OK: <line>; otherwise output exactly NONE."
(cd "$PROBE_CWD" && timeout 60 claude -p --output-format stream-json --verbose "$PROMPT" </dev/null) > run-a.json
(cd "$PROBE_CWD" && CLAUDE_CODE_DISABLE_AUTO_MEMORY=1 timeout 60 claude -p --output-format stream-json --verbose "$PROMPT" </dev/null) > run-b.json

jq -rs '[.[] | select(.type=="assistant")][-1].message.content[] | select(.type=="text") | .text' run-a.json
jq -rs '[.[] | select(.type=="assistant")][-1].message.content[] | select(.type=="text") | .text' run-b.json
jq -cs '[.[] | select(.subtype=="init")][0] | {apiKeySource, memory_paths}' run-a.json
jq -cs '[.[] | select(.subtype=="init")][0] | {apiKeySource, memory_paths}' run-b.json

# cleanup (guarded: only ever deletes a path containing the uuid token, under $HOME/.claude/projects)
rm -rf "$(dirname "$MEMORY_DIR")"
rm -rf "$PROBE_CWD"
rmdir "$HOME/.linkly-crew-probes" 2>/dev/null
```
