//! Real `pi` CLI spot-check (contracts-m8.md §F3) — #[ignore] by default;
//! run explicitly (coordinator only, 함정 19):
//!   cargo test -p crew-harness --test real_pi -- --ignored --nocapture
//!
//! One-turn round trip against the real `pi` binary, mirroring the shape of
//! the captured spike (`~/.linkly-crew/pi-spike/rpc2.jsonl`: "Reply with
//! exactly: OK" -> "OK").

use std::time::Duration;

use crew_harness::pi::PiHarness;
use crew_harness::{AgentCfg, Harness, TurnOutcome, UserTurn};

fn agent_cfg() -> AgentCfg {
    AgentCfg {
        cwd: std::env::current_dir().expect("current dir"),
        model: None,
    }
}

#[tokio::test]
#[ignore]
async fn one_turn_round_trip() {
    let harness = PiHarness::new();
    let cfg = agent_cfg();

    let mut session = harness.spawn(&cfg).await.expect("spawn pi should succeed");
    let mut events = harness.take_events(&mut session);

    let outcome = harness
        .send(
            &mut session,
            UserTurn {
                text: "Reply with exactly: OK".to_string(),
            },
            Duration::from_secs(60),
        )
        .await
        .expect("send should not error at the transport level");

    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
        println!("event: {event:?}");
    }

    assert_eq!(outcome, TurnOutcome::Success, "one-turn round trip should succeed");

    harness.shutdown(session).await.expect("shutdown");
}
