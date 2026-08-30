//! 3 fake-CLI-driven scenarios (D7): normal, forced error, timeout.
//! Each spawns `fake-claude.sh` in place of the real `claude` binary, so
//! the suite is deterministic and free.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;

use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::{AgentCfg, Harness, HarnessEvent, TurnOutcome, UserTurn};

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude.sh")
}

fn agent_cfg() -> AgentCfg {
    AgentCfg {
        cwd: std::env::current_dir().expect("current dir"),
        model: None,
    }
}

#[tokio::test]
async fn normal_turn_is_finished_ok_and_streams_events() {
    let harness = ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
        .with_env("FAKE_MODE", "normal");
    let cfg = agent_cfg();

    let mut session = harness.spawn(&cfg).await.expect("spawn should succeed");
    let mut events = harness.take_events(&mut session);

    let outcome = harness
        .send(
            &mut session,
            UserTurn {
                text: "hello".to_string(),
            },
            Duration::from_secs(5),
        )
        .await
        .expect("send should not error");

    assert_eq!(outcome, TurnOutcome::Success);

    // Drain the events this turn produced and check the normalization
    // pipeline actually ran end to end, not just judge_result in isolation.
    let mut saw_started = false;
    let mut saw_text = false;
    let mut saw_usage = false;
    let mut saw_finished = false;
    while let Ok(event) = tokio::time::timeout(Duration::from_secs(1), events.recv()).await {
        match event {
            Some(HarnessEvent::Started { .. }) => saw_started = true,
            Some(HarnessEvent::Text { delta }) => {
                assert_eq!(delta, "ok");
                saw_text = true;
            }
            Some(HarnessEvent::Usage {
                input_tokens,
                output_tokens,
            }) => {
                assert_eq!(input_tokens, 10);
                assert_eq!(output_tokens, 5);
                saw_usage = true;
            }
            Some(HarnessEvent::Finished { ok, reason }) => {
                assert!(ok);
                assert_eq!(reason, None);
                saw_finished = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_started, "expected a Started event");
    assert!(saw_text, "expected a Text event");
    assert!(saw_usage, "expected a Usage event");
    assert!(saw_finished, "expected a Finished event");

    harness.shutdown(session).await.expect("shutdown");
}

#[tokio::test]
async fn forced_error_turn_is_failed_even_with_subtype_success() {
    let harness = ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
        .with_env("FAKE_MODE", "error");
    let cfg = agent_cfg();

    let mut session = harness.spawn(&cfg).await.expect("spawn should succeed");
    let mut events = harness.take_events(&mut session);

    let outcome = harness
        .send(
            &mut session,
            UserTurn {
                text: "do the forbidden thing".to_string(),
            },
            Duration::from_secs(5),
        )
        .await
        .expect("send should not error");

    // fake-claude.sh's error mode sets subtype:"success" alongside
    // is_error:true — proving the judgment is_error-driven, not subtype-driven.
    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            error: "forced error".to_string()
        }
    );

    let mut saw_failed = false;
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(1), events.recv()).await
    {
        if let HarnessEvent::Failed { error } = event {
            assert_eq!(error, "forced error");
            saw_failed = true;
            break;
        }
    }
    assert!(saw_failed, "expected a Failed event on the events stream");

    harness.shutdown(session).await.expect("shutdown");
}

#[tokio::test]
async fn slow_turn_times_out_and_kills_the_child() {
    let harness = ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
        .with_env("FAKE_MODE", "timeout");
    let cfg = agent_cfg();

    let mut session = harness.spawn(&cfg).await.expect("spawn should succeed");

    // fake-claude.sh's timeout mode sleeps 5s before replying; inject a
    // short duration so the test doesn't wait for it (D6).
    let outcome = harness
        .send(
            &mut session,
            UserTurn {
                text: "this will never get a reply in time".to_string(),
            },
            Duration::from_millis(200),
        )
        .await
        .expect("send should not error — a timeout is a TurnOutcome, not a HarnessError");

    assert_eq!(
        outcome,
        TurnOutcome::Failed {
            error: "timeout".to_string()
        }
    );

    // The child must actually be killed, not left sleeping in the background.
    let exit = tokio::time::timeout(Duration::from_secs(2), session.wait())
        .await
        .expect("child should exit promptly once killed")
        .expect("wait should succeed");
    assert!(!exit.success(), "killed child should not report success");

    harness.shutdown(session).await.expect("shutdown");
}

/// Drains `events` until a `Started` event arrives, so the reader task has
/// definitely already set `Session::reported_session_id` (it does so
/// before sending the event, D3) by the time the caller proceeds — without
/// this, calling `snapshot` right after `spawn` races the reader task.
async fn wait_for_started(events: &mut mpsc::Receiver<HarnessEvent>) {
    loop {
        match tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("Started event should arrive promptly")
        {
            Some(HarnessEvent::Started { .. }) => return,
            Some(_) => continue,
            None => panic!("events channel closed before a Started event arrived"),
        }
    }
}

#[tokio::test]
async fn snapshot_session_id_matches_spawn_uuid_when_cli_echoes_it_back() {
    // fake-claude.sh's default (no FAKE_SESSION_ID override) echoes back
    // the `--session-id` crew-harness spawned with, mirroring the real
    // CLI — so the reported and spawn uuids are the same value here.
    let harness =
        ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap()).with_env("FAKE_MODE", "normal");
    let cfg = agent_cfg();

    let mut session = harness.spawn(&cfg).await.expect("spawn should succeed");
    let mut events = harness.take_events(&mut session);
    wait_for_started(&mut events).await;

    let snapshot = harness.snapshot(&session).await.expect("snapshot should succeed");
    assert_eq!(snapshot.harness, "claude-code");
    assert_eq!(snapshot.session_id, session.session_id().to_string());
    assert_eq!(snapshot.notes, "");

    harness.shutdown(session).await.expect("shutdown");
}

#[tokio::test]
async fn snapshot_prefers_the_cli_reported_session_id_over_spawn_uuid() {
    // Boundary: the init line reports a session_id different from the
    // spawn uuid (FAKE_SESSION_ID overrides the echoed --session-id) —
    // snapshot must prefer the CLI's reported value (D3).
    let reported = "22222222-2222-4222-8222-222222222222";
    let harness = ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
        .with_env("FAKE_MODE", "normal")
        .with_env("FAKE_SESSION_ID", reported);
    let cfg = agent_cfg();

    let mut session = harness.spawn(&cfg).await.expect("spawn should succeed");
    let mut events = harness.take_events(&mut session);
    wait_for_started(&mut events).await;

    assert_ne!(
        session.session_id().to_string(),
        reported,
        "test setup must actually diverge from the spawn uuid"
    );

    let snapshot = harness.snapshot(&session).await.expect("snapshot should succeed");
    assert_eq!(snapshot.session_id, reported);

    harness.shutdown(session).await.expect("shutdown");
}
