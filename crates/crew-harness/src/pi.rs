//! `pi` adapter (contracts-m8.md §F3) — minimal viable: spawn, turn, terminal
//! judgment, defensive parsing. Not parity with `claude-code` (no
//! set_model/steer/compact/fork/resume — M8 scope).
//!
//! Structure mirrors `claude.rs` (§F3 D1): spawn wires up an events channel
//! (64) and a turn-outcome channel (8), a reader task drains stdout and
//! judges each turn, `drain_stderr` is reused verbatim.
//!
//! Empirical basis (contract's "코디네이터 실측" preamble, `~/.linkly-crew/pi-spike/rpc2.jsonl`):
//! stdin EOF shuts the process down immediately, so `Session.stdin` is kept
//! open for the session's lifetime; `agent_settled` is not trustworthy for
//! turn completion — `agent_end(willRetry:false)` is the terminal backstop
//! when `turn_end` is somehow never observed; dialog-shaped
//! `extension_ui_request`s (select/confirm/input/editor) get an immediate
//! `cancelled` response so the process never stalls waiting on a human.

use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use crate::claude::drain_stderr;
use crate::{
    AgentCfg, HandoffSnapshot, Harness, HarnessError, HarnessEvent, HarnessId, Session,
    TurnOutcome, UserTurn,
};

const HARNESS_ID: HarnessId = HarnessId("pi");
const EVENTS_CHANNEL_CAPACITY: usize = 64;
const TURN_CHANNEL_CAPACITY: usize = 8;

/// `extension_ui_request` methods that block on a response (contract's
/// "실측 3") — everything else (setStatus/setWidget/setTitle/notify/
/// set_editor_text) is fire-and-forget and safely ignored.
const DIALOG_METHODS: &[&str] = &["select", "confirm", "input", "editor"];

/// Adapter for a `pi` CLI on `$PATH` (`@earendil-works/pi-coding-agent`).
pub struct PiHarness;

impl PiHarness {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PiHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Harness for PiHarness {
    fn id(&self) -> HarnessId {
        HARNESS_ID
    }

    async fn spawn(&self, cfg: &AgentCfg) -> Result<Session, HarnessError> {
        let session_id = Uuid::new_v4();

        let mut args: Vec<String> = vec![
            "--mode".to_string(),
            "rpc".to_string(),
            "--no-session".to_string(),
        ];
        if let Some(model) = &cfg.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        let mut command = Command::new("pi");
        command
            .args(&args)
            .current_dir(&cfg.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(HarnessError::Spawn)?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let stdin = Arc::new(Mutex::new(stdin));
        let (events_tx, events_rx) = mpsc::channel(EVENTS_CHANNEL_CAPACITY);
        let (turn_tx, turn_rx) = mpsc::channel(TURN_CHANNEL_CAPACITY);
        let reported_session_id: Arc<OnceLock<String>> = Arc::new(OnceLock::new());

        let reader_task = tokio::spawn(read_events_pi(stdout, stdin.clone(), events_tx, turn_tx));
        let stderr_task = tokio::spawn(drain_stderr(stderr));

        Ok(Session {
            session_id,
            child,
            stdin,
            events_rx: Some(events_rx),
            turn_rx,
            reader_task,
            stderr_task,
            reported_session_id,
        })
    }

    async fn send(
        &self,
        session: &mut Session,
        turn: UserTurn,
        timeout: Duration,
    ) -> Result<TurnOutcome, HarnessError> {
        let line = serde_json::to_string(&json!({
            "type": "prompt",
            "message": turn.text,
        }))
        .expect("UserTurn always serializes to JSON");

        {
            let mut stdin = session.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(HarnessError::Write)?;
            stdin.write_all(b"\n").await.map_err(HarnessError::Write)?;
            stdin.flush().await.map_err(HarnessError::Write)?;
        }

        match tokio::time::timeout(timeout, session.turn_rx.recv()).await {
            Ok(Some(outcome)) => Ok(outcome),
            Ok(None) => Err(HarnessError::ProcessExited),
            Err(_elapsed) => {
                let _ = session.child.start_kill();
                Ok(TurnOutcome::Failed {
                    error: "timeout".to_string(),
                })
            }
        }
    }

    fn take_events(&self, session: &mut Session) -> mpsc::Receiver<HarnessEvent> {
        session
            .events_rx
            .take()
            .expect("take_events called more than once on the same session")
    }

    async fn snapshot(&self, session: &Session) -> Result<HandoffSnapshot, HarnessError> {
        let session_id = session
            .reported_session_id
            .get()
            .cloned()
            .unwrap_or_else(|| session.session_id.to_string());
        Ok(HandoffSnapshot {
            harness: HARNESS_ID.0.to_string(),
            session_id,
            notes: String::new(),
        })
    }

    async fn shutdown(&self, mut session: Session) -> Result<(), HarnessError> {
        session.reader_task.abort();
        session.stderr_task.abort();
        let _ = session.child.start_kill();
        let _ = session.child.wait().await;
        Ok(())
    }
}

/// Reads pi's RPC-mode NDJSON `stdout` (D1/D4), normalizes each line into
/// zero or more [`HarnessEvent`]s, and delivers exactly one [`TurnOutcome`]
/// per turn on `turn_tx`. Generic over `R`/`W` (rather than the concrete
/// `ChildStdout`/`ChildStdin`) so unit tests drive it over
/// `tokio::io::duplex` pairs with no child process (D7).
///
/// Never reads `agent_settled` (empirically unreliable — contract's "실측 2").
/// `agent_end(willRetry:false)` is the terminal backstop: if no `turn_end`
/// was observed for the in-flight turn, it synthesizes a `Failed` outcome
/// so `Harness::send` never hangs until its timeout on a turn the CLI
/// considers over.
async fn read_events_pi<R, W>(
    stdout: R,
    stdin: Arc<Mutex<W>>,
    events_tx: mpsc::Sender<HarnessEvent>,
    turn_tx: mpsc::Sender<TurnOutcome>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(stdout).lines();
    let mut outcome_sent_this_turn = false;

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!("crew-harness: reading pi stdout failed: {err}");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                // Defensive parsing (§13.1) — a non-JSON line never kills
                // the session; skip and keep reading.
                tracing::warn!("crew-harness: skipping unparsable pi event line: {err}");
                continue;
            }
        };

        match value.get("type").and_then(Value::as_str) {
            Some("turn_start") => outcome_sent_this_turn = false,
            Some("message_update") => {
                for event in normalize_message_update(&value) {
                    if events_tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
            Some("message_end") => {
                if let Some(event) = normalize_message_end_usage(&value) {
                    if events_tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
            Some("turn_end") => {
                let (event, outcome) = normalize_turn_end(&value);
                outcome_sent_this_turn = true;
                if events_tx.send(event).await.is_err() {
                    return;
                }
                let _ = turn_tx.send(outcome).await;
            }
            Some("auto_retry_end") => {
                if value.get("success").and_then(Value::as_bool) == Some(false) {
                    let error = value
                        .get("finalError")
                        .and_then(Value::as_str)
                        .unwrap_or("pi auto-retry failed with no detail")
                        .to_string();
                    outcome_sent_this_turn = true;
                    let _ = events_tx
                        .send(HarnessEvent::Failed {
                            error: error.clone(),
                        })
                        .await;
                    let _ = turn_tx.send(TurnOutcome::Failed { error }).await;
                }
            }
            Some("agent_end") => {
                let will_retry = value.get("willRetry").and_then(Value::as_bool).unwrap_or(false);
                if !will_retry && !outcome_sent_this_turn {
                    let error = "agent_end without turn_end".to_string();
                    let _ = events_tx
                        .send(HarnessEvent::Finished {
                            ok: false,
                            reason: Some(error.clone()),
                        })
                        .await;
                    let _ = turn_tx.send(TurnOutcome::Failed { error }).await;
                }
                outcome_sent_this_turn = false;
            }
            Some("extension_ui_request") => {
                let method = value.get("method").and_then(Value::as_str);
                if method.is_some_and(|m| DIALOG_METHODS.contains(&m)) {
                    let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
                    respond_cancelled(&stdin, id).await;
                }
                // Non-dialog methods (setStatus/setWidget/setTitle/notify/
                // set_editor_text) are fire-and-forget spam — ignored.
            }
            // Administrative/unknown types (agent_start, response,
            // message_start, or anything not yet in the contract's table)
            // carry nothing this adapter needs — ignored (defensive parsing).
            _ => {}
        }
    }
}

/// Writes `{"type":"extension_ui_response","id":<id>,"cancelled":true}\n` to
/// stdin (contract's "실측 3" — dialogs must never wait on a human). Best
/// effort: a write failure here doesn't tear down the reader loop, since the
/// turn itself may still complete independently.
async fn respond_cancelled<W: AsyncWrite + Unpin + Send>(stdin: &Arc<Mutex<W>>, id: &str) {
    let response = serde_json::to_string(&json!({
        "type": "extension_ui_response",
        "id": id,
        "cancelled": true,
    }))
    .expect("ui response always serializes to JSON");

    let mut guard = stdin.lock().await;
    if guard.write_all(response.as_bytes()).await.is_err() {
        return;
    }
    if guard.write_all(b"\n").await.is_err() {
        return;
    }
    let _ = guard.flush().await;
}

/// `message_update.assistantMessageEvent.type` -> `HarnessEvent`s (D4).
/// Only `text_delta` (not `text_start`/`text_end`, which would double-emit
/// the same text) and `tool_execution_start` carry payload; any
/// `thinking_*` subtype collapses to a bare `Thinking` marker. Everything
/// else, or a missing `assistantMessageEvent`, yields nothing.
fn normalize_message_update(value: &Value) -> Vec<HarnessEvent> {
    let Some(inner) = value.get("assistantMessageEvent") else {
        return Vec::new();
    };
    match inner.get("type").and_then(Value::as_str) {
        Some("text_delta") => vec![HarnessEvent::Text {
            delta: inner.get("delta").and_then(Value::as_str).unwrap_or_default().to_string(),
        }],
        Some(t) if t.starts_with("thinking") => vec![HarnessEvent::Thinking],
        Some("tool_execution_start") => vec![HarnessEvent::ToolUse {
            name: inner
                .get("toolName")
                .or_else(|| inner.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input: inner
                .get("input")
                .or_else(|| inner.get("args"))
                .cloned()
                .unwrap_or(Value::Null),
        }],
        _ => Vec::new(),
    }
}

/// `message_end`'s assistant `usage` -> `Usage` (D4), when present. pi's
/// usage object uses `input`/`output` keys (unlike claude's
/// `input_tokens`/`output_tokens` — contract's rpc2.jsonl sample).
fn normalize_message_end_usage(value: &Value) -> Option<HarnessEvent> {
    let message = value.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let usage = message.get("usage")?;
    Some(HarnessEvent::Usage {
        input_tokens: usage.get("input").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: usage.get("output").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// `turn_end.message.stopReason` -> `(HarnessEvent, TurnOutcome)` (D4).
/// Anything other than `"stop"` — including `"error"` and any value the
/// contract's table doesn't name — is judged a failure; a turn must always
/// resolve to some outcome, never silently drop (a caller's `send` would
/// otherwise hang until its timeout).
fn normalize_turn_end(value: &Value) -> (HarnessEvent, TurnOutcome) {
    let message = value.get("message");
    let stop_reason = message.and_then(|m| m.get("stopReason")).and_then(Value::as_str);
    match stop_reason {
        Some("stop") => (
            HarnessEvent::Finished {
                ok: true,
                reason: None,
            },
            TurnOutcome::Success,
        ),
        _ => {
            let error = extract_turn_end_error(message, stop_reason);
            (
                HarnessEvent::Finished {
                    ok: false,
                    reason: Some(error.clone()),
                },
                TurnOutcome::Failed { error },
            )
        }
    }
}

fn extract_turn_end_error(message: Option<&Value>, stop_reason: Option<&str>) -> String {
    if let Some(error) = message.and_then(|m| m.get("error")).and_then(Value::as_str) {
        return error.to_string();
    }
    format!("pi turn ended with stopReason: {}", stop_reason.unwrap_or("unknown"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    const FIXTURE_NORMAL: &str = include_str!("../tests/fixtures/pi-rpc2.jsonl");
    const FIXTURE_ERROR: &str = include_str!("../tests/fixtures/pi-error.jsonl");
    const FIXTURE_RETRY_FAIL: &str = include_str!("../tests/fixtures/pi-retry-fail.jsonl");
    const FIXTURE_DIALOG: &str = include_str!("../tests/fixtures/pi-dialog.jsonl");
    const FIXTURE_GARBAGE: &str = include_str!("../tests/fixtures/pi-garbage.jsonl");
    const FIXTURE_AGENT_END_BACKSTOP: &str = include_str!("../tests/fixtures/pi-agent-end-backstop.jsonl");

    /// Feeds `fixture` through [`read_events_pi`] with no child process
    /// (D7): a `tokio::io::duplex` stands in for stdout, another for the
    /// stdin the reader task writes dialog responses to. Returns every
    /// event, every turn outcome, and whatever bytes were written to stdin.
    async fn run_fixture(fixture: &str) -> (Vec<HarnessEvent>, Vec<TurnOutcome>, Vec<u8>) {
        let (mut stdout_writer, stdout_reader) = tokio::io::duplex(1 << 16);
        let (stdin_writer, mut stdin_reader) = tokio::io::duplex(1 << 16);
        let stdin = Arc::new(Mutex::new(stdin_writer));

        let (events_tx, mut events_rx) = mpsc::channel(64);
        let (turn_tx, mut turn_rx) = mpsc::channel(8);

        let fixture_owned = fixture.to_string();
        let writer = tokio::spawn(async move {
            stdout_writer.write_all(fixture_owned.as_bytes()).await.unwrap();
            // Dropping stdout_writer here closes the reader's stdout half,
            // so read_events_pi sees EOF and returns.
        });
        let reader = tokio::spawn(read_events_pi(stdout_reader, stdin, events_tx, turn_tx));

        writer.await.expect("writer task should not panic");
        reader.await.expect("reader task should not panic");

        let mut events = Vec::new();
        while let Ok(event) = events_rx.try_recv() {
            events.push(event);
        }
        let mut outcomes = Vec::new();
        while let Ok(outcome) = turn_rx.try_recv() {
            outcomes.push(outcome);
        }

        // The reader task's stdin Arc was dropped when it returned above,
        // so this reads whatever was written (if anything) and then EOFs.
        let mut captured = Vec::new();
        let _ = tokio::time::timeout(Duration::from_millis(200), stdin_reader.read_to_end(&mut captured))
            .await
            .expect("stdin capture should EOF promptly once the reader task exits");

        (events, outcomes, captured)
    }

    #[tokio::test]
    async fn normal_turn_maps_to_success_with_text_and_usage() {
        let (events, outcomes, stdin_written) = run_fixture(FIXTURE_NORMAL).await;

        assert_eq!(outcomes, vec![TurnOutcome::Success]);
        assert!(
            events.iter().any(|e| matches!(e, HarnessEvent::Text { delta } if delta == "OK")),
            "expected a Text{{delta:\"OK\"}} event, got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, HarnessEvent::Usage { input_tokens, output_tokens }
                    if *input_tokens == 6750 && *output_tokens == 5)),
            "expected the usage event from message_end, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, HarnessEvent::Finished { ok: true, reason: None })),
            "expected Finished{{ok:true}}, got {events:?}"
        );
        assert!(stdin_written.is_empty(), "no dialog in this fixture, nothing should be written to stdin");
    }

    #[tokio::test]
    async fn stop_reason_error_maps_to_failed() {
        let (events, outcomes, _stdin_written) = run_fixture(FIXTURE_ERROR).await;

        assert_eq!(
            outcomes,
            vec![TurnOutcome::Failed {
                error: "forced error".to_string()
            }]
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, HarnessEvent::Finished { ok: false, reason: Some(r) } if r == "forced error")),
            "expected Finished{{ok:false}} carrying the error, got {events:?}"
        );
    }

    #[tokio::test]
    async fn auto_retry_end_failure_maps_to_failed() {
        let (events, outcomes, _stdin_written) = run_fixture(FIXTURE_RETRY_FAIL).await;

        assert_eq!(
            outcomes,
            vec![TurnOutcome::Failed {
                error: "rate limited after 3 attempts".to_string()
            }]
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, HarnessEvent::Failed { error } if error == "rate limited after 3 attempts")),
            "expected a Failed event carrying finalError, got {events:?}"
        );
    }

    #[tokio::test]
    async fn dialog_ui_request_gets_an_immediate_cancelled_response() {
        let (_events, outcomes, stdin_written) = run_fixture(FIXTURE_DIALOG).await;

        let written = String::from_utf8(stdin_written).expect("response should be valid utf8");
        let response: Value = serde_json::from_str(written.trim()).expect("response should be one JSON line");
        assert_eq!(response["type"], "extension_ui_response");
        assert_eq!(response["id"], "dialog-1");
        assert_eq!(response["cancelled"], true);

        // The turn itself must still resolve normally alongside the dialog.
        assert_eq!(outcomes, vec![TurnOutcome::Success]);
    }

    #[tokio::test]
    async fn garbage_lines_and_unknown_types_are_harmless() {
        let (events, outcomes, _stdin_written) = run_fixture(FIXTURE_GARBAGE).await;

        assert_eq!(
            outcomes,
            vec![TurnOutcome::Success],
            "the non-JSON line and the unknown event type must not derail the turn that follows"
        );
        assert!(events.iter().any(|e| matches!(e, HarnessEvent::Finished { ok: true, .. })));
    }

    #[tokio::test]
    async fn agent_end_without_turn_end_is_a_terminal_backstop() {
        let (events, outcomes, _stdin_written) = run_fixture(FIXTURE_AGENT_END_BACKSTOP).await;

        assert_eq!(
            outcomes,
            vec![TurnOutcome::Failed {
                error: "agent_end without turn_end".to_string()
            }]
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, HarnessEvent::Finished { ok: false, reason: Some(r) } if r == "agent_end without turn_end")),
            "expected the backstop Finished event, got {events:?}"
        );
    }

    #[test]
    fn id_is_pi() {
        assert_eq!(PiHarness::new().id().0, "pi");
    }
}
