//! Real `claude` CLI spike (D7/D8) — #[ignore] by default; run explicitly:
//!   cargo test -p crew-harness --test real_claude -- --ignored --nocapture
//!
//! Validates the M1 success criterion (DESIGN.md §11): one process, 3
//! consecutive turns, each received as structured events, plus a resume.
//! Results (success or failure, as observed) are recorded in SPIKE.md.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::{AgentCfg, Harness, HarnessEvent, TurnOutcome, UserTurn};

fn agent_cfg() -> AgentCfg {
    AgentCfg {
        cwd: std::env::current_dir().expect("current dir"),
        model: None,
        tool_use: false,
    }
}

async fn drain_briefly(events: &mut tokio::sync::mpsc::Receiver<crew_harness::HarnessEvent>, label: &str) {
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
        println!("  [{label}] event: {event:?}");
    }
}

#[tokio::test]
#[ignore]
async fn three_consecutive_turns_then_resume() {
    let harness = ClaudeCodeHarness::new();
    let cfg = agent_cfg();

    let mut session = harness.spawn(&cfg).await.expect("spawn claude should succeed");
    let session_id = session.session_id();
    println!("spawned session_id={session_id}");

    let mut events = harness.take_events(&mut session);

    let prompts = [
        "Reply with exactly the single word: one",
        "Reply with exactly the single word: two",
        "Reply with exactly the single word: three",
    ];

    for (i, text) in prompts.iter().enumerate() {
        let turn_no = i + 1;
        let outcome = harness
            .send(
                &mut session,
                UserTurn {
                    text: text.to_string(),
                },
                Duration::from_secs(60),
            )
            .await
            .expect("send should not error at the transport level");
        println!("turn {turn_no} outcome: {outcome:?}");
        drain_briefly(&mut events, &format!("turn {turn_no}")).await;
        assert_eq!(
            outcome,
            TurnOutcome::Success,
            "turn {turn_no} should succeed"
        );
    }

    harness.shutdown(session).await.expect("shutdown");

    // D8: resume must be validated against the real CLI (fake CLI has no
    // persisted session state).
    let mut resumed = harness
        .spawn_resumed(&cfg, session_id)
        .await
        .expect("resume should succeed");
    let mut resumed_events = harness.take_events(&mut resumed);

    let outcome = harness
        .send(
            &mut resumed,
            UserTurn {
                text: "What was the first word I asked you to reply with, verbatim?".to_string(),
            },
            Duration::from_secs(60),
        )
        .await
        .expect("send should not error at the transport level");
    println!("resume turn outcome: {outcome:?}");
    drain_briefly(&mut resumed_events, "resume").await;
    assert_eq!(outcome, TurnOutcome::Success, "resumed turn should succeed");

    harness.shutdown(resumed).await.expect("shutdown");
}

/// Trap 8 real-CLI smoke — coordinator-manual only, workers must not run it
/// (trap 19):
///   cargo test -p crew-harness --test real_claude -- --ignored --nocapture \
///     spawned_session_fires_no_user_global_hooks
/// Precondition: `claude` >= 2.1.236, and the user-global
/// `~/.claude/settings.json` has at least one hook configured.
/// Assertion: the spawned session's stream-json carries zero
/// `hook_started` events (D9) — the default `--setting-sources
/// project,local` (ClaudeCodeHarness::new) keeps the user-global source,
/// and therefore its hooks, out of the child.
#[tokio::test]
#[ignore]
async fn spawned_session_fires_no_user_global_hooks() {
    let harness = ClaudeCodeHarness::new();
    let cfg = agent_cfg();

    let mut session = harness.spawn(&cfg).await.expect("spawn claude should succeed");
    let mut events = harness.take_events(&mut session);

    // Hooks fire at SessionStart, before any turn is sent, so draining here
    // (before `send`) already observes everything they would produce.
    let mut hook_started_count = 0usize;
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(3), events.recv()).await {
        if let HarnessEvent::Raw(value) = &event {
            if value.get("subtype").and_then(|s| s.as_str()) == Some("hook_started") {
                hook_started_count += 1;
            }
        }
    }
    assert_eq!(
        hook_started_count, 0,
        "spawned session must fire no user-global hooks"
    );

    harness.shutdown(session).await.expect("shutdown");
}

// ---------------------------------------------------------------------------
// `AgentCfg::tool_use` real-CLI spot check (issue #31, design D11-D13)
// ---------------------------------------------------------------------------

/// Removes the run directory even when an assertion panics (D13). The scratch
/// ROOT is removed with `remove_dir` (never `remove_dir_all`) and only when this
/// test created it, so a scratch root that already held someone else's files is
/// never touched — `remove_dir` refuses a non-empty directory.
struct ScratchDir {
    run_dir: PathBuf,
    root: PathBuf,
    remove_root: bool,
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.run_dir);
        if self.remove_root {
            let _ = std::fs::remove_dir(&self.root);
        }
    }
}

/// Resolves a `Write` call's `file_path` to a comparable absolute path: a
/// relative path is joined onto `cwd`, then the PARENT is canonicalized and the
/// file name re-appended (the file itself must not exist, so the whole path
/// cannot be canonicalized). A missing `file_path`, a parent that cannot be
/// canonicalized, or a path with no file name yields `None` — read as "does not
/// match the target", never a panic (D12).
fn resolved_write_target(input: &serde_json::Value, cwd: &Path) -> Option<PathBuf> {
    let raw = input.get("file_path")?.as_str()?;
    let joined = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };
    let parent = joined.parent()?.canonicalize().ok()?;
    Some(parent.join(joined.file_name()?))
}

/// A `tool_result` block's text: `content` is either a JSON string or an array
/// of blocks whose `"text"` fields are concatenated (D12).
fn tool_result_text(block: &serde_json::Value) -> String {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        other => format!("{other:?}"),
    }
}

/// The JSON object inside one reply block: strips a surrounding ``` fence, then
/// takes the first `{` to the last `}` of THAT block only (D11 — spanning all
/// deltas would let an earlier block's braces corrupt the parse).
fn json_object_from_block(block: &str) -> String {
    let mut body = block.trim();
    if let Some(rest) = body.strip_prefix("```json") {
        body = rest.trim_start();
    } else if let Some(rest) = body.strip_prefix("```") {
        body = rest.trim_start();
    }
    if let Some(rest) = body.strip_suffix("```") {
        body = rest.trim_end();
    }
    match (body.find('{'), body.rfind('}')) {
        (Some(start), Some(end)) if end > start => body[start..=end].to_string(),
        _ => body.to_string(),
    }
}

/// Real-CLI proof that `tool_use: true` lets the CLI write inside `cwd` and gets
/// an out-of-cwd write DENIED (design D11-D13). The fake-CLI suite proves the
/// flags are PASSED; only the real CLI proves what they DO — wiki
/// testing-strategy-real-cli-spot-check-for-new-execution-paths.
///
/// Scope: WRITES only. This test measures nothing about reads, and reads are not
/// confined by these flags — see [`crew_harness::AgentCfg::tool_use`]. Do not
/// read a pass here as evidence that out-of-cwd reads are blocked.
///
/// Spends real API calls; at most 3 runs per change. Run explicitly:
///   cargo test -p crew-harness --test real_claude tool_use_writes_inside_cwd_and_is_denied_outside -- --ignored --nocapture
#[tokio::test]
#[ignore]
async fn tool_use_writes_inside_cwd_and_is_denied_outside() {
    // Scratch under the worktree, never /tmp or $TMPDIR (D13). NOT under
    // `.claude/` : the CLI classifies any path there as a sensitive file, so
    // `acceptEdits` still demands an approval that `--permission-prompts none`
    // then denies — measured 2026-09-28 with the same flags in both arms (cwd
    // under `target/` wrote the file; cwd under `.claude/tmp` was refused with
    // "it counts as a sensitive file"). D11 fallback (a), approved.
    let scratch_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/t5-toolwrite");
    let root_existed = scratch_root.exists();
    std::fs::create_dir_all(&scratch_root).expect("create scratch root");
    let scratch_root = scratch_root.canonicalize().expect("canonicalize scratch root");
    let run_dir = scratch_root.join(format!(
        "t5-toolwrite-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    assert!(
        run_dir.starts_with(&scratch_root),
        "run dir {} must stay under the scratch root {}",
        run_dir.display(),
        scratch_root.display()
    );
    std::fs::create_dir_all(run_dir.join("cwd")).expect("create run cwd");
    let run_dir = run_dir.canonicalize().expect("canonicalize run dir");
    let cwd = run_dir.join("cwd");
    let _guard = ScratchDir {
        run_dir: run_dir.clone(),
        root: scratch_root.clone(),
        remove_root: !root_existed,
    };

    // No new AgentCfg literal (D3): mutate the helper's cfg.
    let mut cfg = agent_cfg();
    cfg.tool_use = true;
    cfg.cwd = cwd.clone();

    let harness = ClaudeCodeHarness::new();
    let mut session = harness.spawn(&cfg).await.expect("spawn claude should succeed");
    println!("run_dir={}", run_dir.display());
    println!("cwd={}", cwd.display());

    // Drain CONCURRENTLY (D11): a tool-using turn can outgrow the 64-slot event
    // channel, and draining only after `send` returns would deadlock.
    let mut events = harness.take_events(&mut session);
    let collector = tokio::spawn(async move {
        let mut all = Vec::new();
        while let Some(event) = events.recv().await {
            let terminal = matches!(
                event,
                HarnessEvent::Finished { .. } | HarnessEvent::Failed { .. }
            );
            all.push(event);
            if terminal {
                break;
            }
        }
        all
    });

    let outside = run_dir.join("outside.txt");
    let text = format!(
        "This is a file-permission test. Use your file tools now, one tool call at a time; never issue two tool calls in parallel.\n\
         1. Create the file hello.txt in the current directory with exactly this content: t5-toolwrite-ok\n\
         2. After step 1 has finished, make one separate Write tool call to create the file {outside} (it is in the parent directory of the current directory) with exactly this content: must-not-exist\n\
         \x20  Attempt step 2 with the Write tool even if you expect it to be refused. If it is refused, do not retry it in any other way and do not write that file anywhere else.\n\
         3. Then reply ONLY with JSON {{\"covered_req_ids\": [], \"artifacts\": [{{\"name\": \"hello\", \"kind\": \"text\", \"req_ids\": [], \"path\": \"hello.txt\"}}]}} where \"path\" is relative to the current directory.",
        outside = outside.display()
    );

    let start = Instant::now();
    let outcome = harness
        .send(&mut session, UserTurn { text }, crew_harness::DEFAULT_TURN_TIMEOUT)
        .await
        .expect("send should not error at the transport level");
    let wall = start.elapsed();
    let all = collector.await.expect("collector task");

    // Print the evidence BEFORE asserting, so a failing run still records what
    // the real CLI emitted (the DoD record is this output).
    println!(
        "WALL_TIME_SECS={:.1} TURN_TIMEOUT_SECS={}",
        wall.as_secs_f64(),
        crew_harness::DEFAULT_TURN_TIMEOUT.as_secs()
    );
    println!("outcome={outcome:?}");
    for event in &all {
        match event {
            HarnessEvent::ToolUse { name, input } => println!("TOOL_USE name={name} input={input}"),
            HarnessEvent::Raw(v) if v.get("type").and_then(|t| t.as_str()) == Some("user") => {
                let dump = v.to_string();
                let shown: String = dump.chars().take(400).collect();
                println!("RAW_USER {shown}");
            }
            _ => {}
        }
    }

    // (a) the turn itself succeeded
    assert_eq!(outcome, TurnOutcome::Success, "the turn must succeed");

    // (b) the in-cwd write landed with the fixed content
    let hello = std::fs::read_to_string(cwd.join("hello.txt"))
        .expect("hello.txt must exist inside cwd after an acceptEdits turn");
    assert_eq!(
        hello.trim_end(),
        "t5-toolwrite-ok",
        "hello.txt content must be the instructed literal"
    );

    // (c) the out-of-cwd write landed nowhere
    assert!(
        !outside.exists(),
        "{} must not exist — the out-of-cwd write must be denied",
        outside.display()
    );
    assert!(
        !cwd.join("outside.txt").exists(),
        "outside.txt must not be written into cwd as a fallback either"
    );

    // (d) D12 (1): the denial is not vacuous — exactly one Write actually aimed
    // at <run_dir>/outside.txt. Zero attempts would make (c) pass on its own.
    let attempts: Vec<usize> = all
        .iter()
        .enumerate()
        .filter_map(|(i, event)| match event {
            HarnessEvent::ToolUse { name, input } if name == "Write" => {
                resolved_write_target(input, &cwd).filter(|p| *p == outside).map(|_| i)
            }
            _ => None,
        })
        .collect();
    let attempt_idx = match attempts.len() {
        1 => attempts[0],
        0 => panic!(
            "inconclusive: no Write to {} was attempted — the denial assertion would pass vacuously",
            outside.display()
        ),
        n => panic!("inconclusive: repeated attempt — {n} Writes aimed at {}", outside.display()),
    };

    // (e) D12 (3): its paired tool_result is an error whose text says permission.
    // `HarnessEvent::ToolUse` carries no tool_use id, so the result is read from
    // the Raw `type:"user"` events in the window after the attempt.
    let mut blocks = Vec::new();
    for event in all.iter().skip(attempt_idx + 1) {
        match event {
            HarnessEvent::ToolUse { .. }
            | HarnessEvent::Finished { .. }
            | HarnessEvent::Failed { .. } => break,
            HarnessEvent::Raw(v) if v.get("type").and_then(|t| t.as_str()) == Some("user") => {
                if let Some(content) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            blocks.push(block.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        blocks.len(),
        1,
        "inconclusive: cannot pair the tool_result for the out-of-cwd Write (found {} blocks)",
        blocks.len()
    );
    let block = &blocks[0];
    assert_eq!(
        block.get("is_error").and_then(|v| v.as_bool()),
        Some(true),
        "the out-of-cwd Write's tool_result must be an error, got: {block}"
    );
    let result_text = tool_result_text(block);
    assert!(
        result_text.to_lowercase().contains("permission"),
        "the denial must be a permission denial, got: {result_text}"
    );

    // (f) the reply reports the artifact by relative path
    let last_text = all
        .iter()
        .rev()
        .find_map(|event| match event {
            HarnessEvent::Text { delta } => Some(delta.clone()),
            _ => None,
        })
        .expect("the turn must produce at least one Text block");
    let json_text = json_object_from_block(&last_text);
    let reply: serde_json::Value =
        serde_json::from_str(&json_text).unwrap_or_else(|e| panic!("reply is not JSON ({e}): {json_text}"));
    assert_eq!(
        reply["artifacts"][0]["path"], "hello.txt",
        "artifacts[0].path must be the cwd-relative path, got: {reply}"
    );

    harness.shutdown(session).await.expect("shutdown");
}
