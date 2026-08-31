# SPIKE-M10 — Cmd DoD 프로세스 그룹 & 플래너 방출 실측

**측정자**: 코디네이터 (오케스트레이션 런 m10a)
**측정일**: 2026-08-31
**측정 대상 커밋**: `crew-m10-integration` (t-pgroup `6108698` + t-cmdplan `2947ad1`, 머지 `cf342f5`)
**환경**: macOS(darwin 25.1.0), Apple Silicon, rustc/cargo 워크스페이스 로컬, tokio 1.53.1

---

## §1 — `tokio::process::Command::process_group` 가용성 (핀 근거)

| 질문 | 실측 결과 | 근거 |
|---|---|---|
| 이 API가 현재 해소되는 tokio에 있는가 | **있다** | `~/.cargo/registry/.../tokio-1.53.1/src/process/mod.rs:790` `pub fn process_group(&mut self, pgroup: i32)`; 바로 위 788행이 `#[cfg(unix)]` |
| 언제부터 있는가 | **1.22.0 추가(unstable) → 1.40.0 stabilize** | tokio `CHANGELOG.md`: 2102행 `process: add Command::process_group (#5114)`가 `# 1.22.0 (November 17, 2022)` 구간, 1043행 `process: stabilize Command::process_group (#6731)`가 `# 1.40.0 (August 30th, 2024)` 구간 |
| 기존 핀으로 충분했는가 | **아니다** | 루트 `Cargo.toml`이 `tokio = { version = "1" }`이었다. Cargo.lock은 gitignore돼 있어(`.gitignore:8`) 다른 머신의 fresh resolve가 1.40 미만을 고를 수 있었다 |

**귀결**: M10은 `process_group`을 호출하는 **같은 변경 안에서** 핀을 `"1.40"`으로 올렸다.
근거 위키: `platforms/toolchains/flag-availability-at-the-execution-site` — 새 API 호출과
그것을 보장하는 버전 핀은 같은 변경이어야 드리프트하지 않는다.

외부 1차 출처:
- rust-lang/rust#115241 "Child::kill will not terminate children on Linux" —
  <https://github.com/rust-lang/rust/issues/115241>
  ("`.process_group(0)` will set child's process group to its PID. After spawning the
  command, it will be possible to call `kill(-pid, SIGKILL)`, which will terminate the
  child and all its children")
- tokio-rs/tokio#2504 — `Child::kill`이 직계 자식에게만 SIGKILL을 보낸다는 확인:
  <https://github.com/tokio-rs/tokio/issues/2504>

---

## §2 — 함정 26 재현: 수정 전에는 손자가 살아남는다

`crates/crew-lead/src/cmd_exec.rs`를 M9 의미론으로 되돌려(= `process_group(0)` 제거 +
가드 미구성 → 타임아웃 시 `child.kill()`) 같은 테스트를 돌린 A/B 측정.

테스트(`timed_out_command_kills_grandchild_process`)가 띄우는 프로세스 트리:

```
execute_cmd_checks
└─ /bin/sh spawner.sh          (직계 자식 — child.kill()이 죽이는 유일한 프로세스)
   ├─ sleep 300 &              (백그라운드 손자 — pid를 파일에 기록, 테스트가 관측)
   └─ sleep 300                (포그라운드 손자 — 테스트가 추적하지 않는다)
```

| 실행 | 테스트 exit | 손자 관측 (`kill(pid,0)`) | 테스트 종료 후 남은 `sleep 300` |
|---|---|---|---|
| **FIX ON** (`process_group(0)` + `killpg`) | `0` | ESRCH (죽음) | **0개** |
| **FIX OFF** (M9 의미론) | `101` | **0 반환 = 살아있음** | **1개** (pid 1309, 포그라운드 손자) |

FIX OFF의 단언 실패 원문 (코디네이터가 직접 재현):
```
thread 'cmd_exec::tests::timed_out_command_kills_grandchild_process' panicked at
crates/crew-lead/src/cmd_exec.rs:678:9:
assertion `left == right` failed: expected grandchild pid 98973 to be dead after the
timeout, but kill(pid, 0) returned 0 (still alive)
  left: 0
 right: -1
```

**측정으로 드러난 추가 사실**: 수정 전 누수는 기록된 손자 1개가 아니라 **자손 전부**다.
테스트의 `TestCleanup`은 자기가 기록한 백그라운드 손자만 정리하므로, RED 실행 뒤에도
스크립트의 **포그라운드 손자 1개가 그대로 남았다**(위 표의 마지막 열). 즉 함정 26이
말한 "손자 프로세스가 남는다"는 단수가 아니라 트리 전체이며, `cargo test`/`npm test`처럼
자기 자식을 여럿 낳는 런처에서는 그만큼 `target/` 락 보유자가 늘어난다.

수정 후 A/B를 되돌린 뒤 파일이 커밋 상태와 바이트 동일함을 확인했다
(`git status --short crates/crew-lead/src/cmd_exec.rs` → 0줄).

### §2.1 플레이크 여부 (좀비 창)
`killpg` 직후 손자는 재부모화·수확 전까지 잠깐 좀비이고 그동안 `kill(pid,0)`이 0을 낼 수
있다. 실측: **손자 테스트 단독 15회 연속 15/15 통과**, **`cargo test -p crew-lead` 전체
스위트 3회 3/3 통과**(병렬 부하). 현 시점 플레이크 관측 0건.

### §2.2 잔여 한계
`process_group(0)`은 자식을 새 그룹의 리더로 만들 뿐이다. 손자가 **스스로** `setsid()`나
`setpgid()`로 새 세션/그룹을 만들면 `killpg`의 사정거리 밖으로 나간다.
`cargo`/`npm`/`pnpm`/`yarn`은 그러지 않으므로 허용목록 범위 안에서는 영향이 없다.

---

## §3 — 플래너 방출(§4-1)이 실 런에서 마주치는 벽: 실행 cwd에 프로젝트가 없다

`with_cmd_exec`가 넘기는 실행 cwd는 `crew-run`의 `role_cli_cwd(data_dir, role)` =
`<data_dir>/cli-cwd/<role>`이고, 앱의 `data_dir`은 `~/.linkly-crew/app-runs/<launch_id>`다.
그 경로의 어떤 상위에도 `Cargo.toml`이 없다(`~/Cargo.toml`·`/Cargo.toml` 부재 확인).

실측 — 그 모양의 디렉토리를 만들고 `cargo test`를 실행:
```
$ cd ~/.linkly-crew/m10-spike-cwd/cli-cwd/developer && cargo test ; echo $?
error: could not find `Cargo.toml` in `.../cli-cwd/developer` or any parent directory
101
```

**귀결**: 지금 `dev_cmd_checks`를 켜면 `CmdOutcome::Ran { exit_code: 101 }`이 나오고
`expect "exit 0"`과 불일치해 `DodVerdict.failed_cmds`에 쌓여 리워크 루프로 간다.
이것이 M10에서 **기본값을 OFF로 둔 이유**이며, 노브가 실제로 쓸모 있어지려면 먼저
"에이전트의 작업 cwd = 실제 프로젝트 루트"가 성립해야 한다. 후속 마일스톤 과제다.

### §3.1 테스트 쪽의 역방향 위험 (t-cmdplan이 잡은 것)
반대로 **테스트 안에서는** `data_dir`이 `crates/crew-run` 아래에 중첩되므로 cargo가
상위로 올라가 **crew-run 자신의 `Cargo.toml`을 찾는다**. 파싱되는 `expect`를 쓴 통합
테스트는 바깥 `cargo test` 안에서 중첩 `cargo test`를 띄워 같은 `target/` 락을 다투게 된다.
`crates/crew-run/tests/m10_cmd_dod.rs`가 파싱 불가 sentinel `expect`를 쓰는 이유이며,
근거는 M9의 `parse_expect` 계약(파싱 불가 = 아예 실행 안 함)이다.

---

## §4 — 통합 스위트 실측

| 워크트리 | `cargo test --workspace` | app `cargo check` |
|---|---|---|
| t-pgroup (`6108698`) | rc=0, **360 passed / 0 failed** (베이스라인 357 + 신규 3) | rc=0 |
| t-cmdplan (`2947ad1`) | rc=0, **371 passed / 0 failed** (베이스라인 357 + 신규 14) | rc=0 |

테스트 삭제 0건 — 파일별 테스트 수를 베이스라인과 직접 대조해 확인했다
(`cmd_exec.rs` 17→20, `plan.rs` 13→20, `config.rs` 0→3, `controller.rs` 8→9,
`m10_cmd_dod.rs` 0→3, 기존 5개 통합 테스트 파일 수량 불변).
