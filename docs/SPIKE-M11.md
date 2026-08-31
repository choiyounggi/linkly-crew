# SPIKE-M11 — npm 간접 실행 함정(29) 재현 + 실행 cwd A/B + 통합 스위트 실측

**측정 대상 커밋**: `crew-m11-integration` `49509dc`(t-vet `de68636` + t-cwd `35e6736` 머지)
**환경**: macOS(darwin 25.1.0), Apple Silicon

---

## §1 — npm 간접 실행: `--ignore-scripts`도 함정 29를 막지 못한다

**측정자**: 코디네이터 (워크트리 밖, `~/.linkly-crew/m11-attack-probe`)
**측정일**: 2026-08-31T09:28:39Z (UTC)
**도구 버전**: npm 11.19.0 / node v26.7.0 / macOS darwin 25.1.0

목적: HANDOFF §5 함정 29("`npm|yarn|pnpm run <script>`의 간접 실행이 허용목록을 무력화한다")의
"완화 방향(선택지, 미확정)" 중 `--ignore-scripts`가 이 경로를 막을 수 있는지 확인한다.

프로브 디렉토리(레포 밖): `~/.linkly-crew/m11-attack-probe`
```
{
  "name": "probe", "version": "1.0.0",
  "scripts": {
    "build": "echo ARBITRARY_CODE_EXECUTED > ./pwned-build.txt; exit 0",
    "test":  "echo ARBITRARY_CODE_EXECUTED > ./pwned-test.txt; exit 0"
  }
}
```

각 명령 전 `rm -f pwned-build.txt pwned-test.txt`로 초기화. stderr는 네 번 모두 비어 있었다.
stdout(4건 공통 서식):
```
> probe@1.0.0 <script>
> echo ARBITRARY_CODE_EXECUTED > ./pwned-<script>.txt; exit 0
```

| 명령 | rc | 생성된 파일 |
|---|---|---|
| `npm run build` | 0 | `pwned-build.txt` |
| `npm run build --ignore-scripts` | 0 | `pwned-build.txt` |
| `npm test` | 0 | `pwned-test.txt` |
| `npm test --ignore-scripts` | 0 | `pwned-test.txt` |

**결론**: `--ignore-scripts`는 `npm install`류의 라이프사이클 훅(`preinstall`/`postinstall` 등)을
막는 플래그이지, `npm run <script>`/`npm test`로 **명시 호출된** 스크립트 자체는 막지 않는다.
네 실행 모두 `pwned-*.txt`가 생성됐고 rc=0이었다 — 함정 29의 간접 실행은 `--ignore-scripts`로
닫히지 않는다.

### §1.1 플레이크 확인 (함정 20 계열 — 채록 하네스 결함, npm 결함 아님)
1차 채록에서 `npm run build`가 rc=1/미실행으로 보였으나, 원인은 zsh `nomatch`가 루프 본문을
중단시킨 **채록 하네스의 결함**이었고 npm이 아니었다. 명령별로 분리 채록해 재현하니 네 건
모두 rc=0으로 일치했다.

### §1.2 이 실측이 닫지 못하는 것
이 실측은 함정 29를 **닫지 않는다** — `--ignore-scripts`라는 죽은 선택지 하나를 소거했을
뿐이다. §5 함정 29 죽은 선택지 목록(HANDOFF.md) 참고.

---

## §2 — 실행 cwd A/B: `project_root` 지정이 M10 벽(exit 101)을 걷는다

**측정자**: 코디네이터 (워크트리 밖)
**측정일**: 2026-08-31
**측정 대상 커밋**: `49509dc`

같은 명령 `cargo test`, cwd만 다름.

**A. `project_root: None`의 실행 cwd 모양 = `<data_dir>/cli-cwd/<role>`**
실측 경로: `~/.linkly-crew/m11-cwd-probe/cli-cwd/developer`(빈 디렉토리)
```
$ cd ~/.linkly-crew/m11-cwd-probe/cli-cwd/developer && cargo test ; echo $?
error: could not find `Cargo.toml` in `.../cli-cwd/developer` or any parent directory
101
```
→ rc=101, stdout 비어 있음.

**B. `project_root: Some(<레포 루트>)`의 실행 cwd = 레포 루트**
→ rc=0, `test result:` 줄 합산 **394 passed / 0 failed / 9 ignored**
(이 합계는 t-docs 머지 전, 49509dc 시점의 값 — §3에서 t-docs가 항진 테스트 1개를 지운 뒤의
재측정값과 다르다. 혼동 금지.)

**결론**: A/B가 §4 잔여 4번("Cmd DoD 실행 cwd를 실제 프로젝트 루트로")이 실제로 벽을 걷었다는
증거다 — SPIKE-M10.md §3이 기록한 exit 101이 바로 A(`project_root` 미지정)이고, B(`project_root`
지정)가 그 벽을 없앤다. **이것은 함정 29를 닫지 않는다** — `project_root`를 지정하면 에이전트의
CLI 세션 cwd와 Cmd DoD 실행 cwd가 여전히 같은 디렉토리(둘 다 `project_root`)이므로, 함정 29가
전제한 "에이전트가 쓴 `package.json`을 DoD 실행기가 그대로 읽는다"는 조건이 **그대로 유지된다**
— 오히려 그 디렉토리가 스크래치 cwd에서 사람이 지정한 실제 트리로 바뀌므로, 무장 범위가 넓어진다.

---

## §3 — 통합 스위트 실측 (t-docs, config.rs/plan.rs 정리 후)

**측정자**: t-docs 워커 (자기 워크트리 `.worktrees/crew-m11-t-docs`)
**측정일**: 2026-08-31
**측정 대상 커밋**: t-docs 워크트리, 베이스 `49509dc` + 이 태스크의 config.rs/plan.rs 편집(미커밋)

| 명령 | 결과 |
|---|---|
| `cargo test --workspace` | rc=0, **393 passed / 0 failed / 9 ignored** |
| `cargo check --all-targets --manifest-path apps/crew-app/src-tauri/Cargo.toml` | rc=0 |

393은 §2의 394에서 이 태스크가 삭제한 항진 테스트 `plan_dag_with_default_options_equals_plan_dag_for`
1개를 뺀 값(394 − 1 = 393)으로, **줄어드는 것이 정상**이다. 검산: M11 이전 베이스(`b658289`)
376 passed + 두 태스크(t-vet 7 + t-cwd 인라인 2 + `m11_project_root.rs` 9 = 18개) − 이 태스크가
지운 1개 = 393.

이 값은 워커 워크트리에서 잰 것이며, 최종 수치 확정은 코디네이터가 머지 후 재측정한다.
