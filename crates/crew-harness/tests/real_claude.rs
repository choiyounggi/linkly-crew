//! Real `claude` CLI spike (D7/D8) — #[ignore] by default; run explicitly:
//!   cargo test -p crew-harness --test real_claude -- --ignored --nocapture
//!
//! Validates the M1 success criterion (DESIGN.md §11): one process, 3
//! consecutive turns, each received as structured events, plus a resume.
//! Results (success or failure, as observed) are recorded in SPIKE.md.

use std::time::Duration;

use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::{AgentCfg, Harness, HarnessEvent, TurnOutcome, UserTurn};

fn agent_cfg() -> AgentCfg {
    AgentCfg {
        cwd: std::env::current_dir().expect("current dir"),
        model: None,
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
