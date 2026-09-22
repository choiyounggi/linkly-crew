//! `claude-code` adapter — spawns `claude -p` as a long-lived stream-json
//! session and normalizes its NDJSON stdout into [`HarnessEvent`]s.
//! Decisions D1-D3, D6, D8, D10, D11 in the plan.

use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use crate::{
    judge_result, AgentCfg, HandoffSnapshot, Harness, HarnessError, HarnessEvent, HarnessId,
    Session, TurnOutcome, UserTurn,
};

const HARNESS_ID: HarnessId = HarnessId("claude-code");
const EVENTS_CHANNEL_CAPACITY: usize = 64;
const TURN_CHANNEL_CAPACITY: usize = 8;
const DEFAULT_SETTING_SOURCES: &str = "project,local";
const AUTO_MEMORY_DISABLE_ENV: &str = "CLAUDE_CODE_DISABLE_AUTO_MEMORY";
const AUTO_MEMORY_DISABLE_VALUE: &str = "1";

/// Adapter for a `claude` CLI on `$PATH` (or another binary of the same
/// stream-json protocol, for fake-CLI-driven tests — see `with_binary`).
pub struct ClaudeCodeHarness {
    claude_bin: String,
    extra_env: Vec<(String, String)>,
    /// `--setting-sources` value. Scopes the settings a spawned session
    /// loads so a user-global hook (`~/.claude/settings.json`) can't fire
    /// inside it (trap 8). Default `Some("project,local")` — excludes only
    /// the user-global source; project/local settings still load. `None`
    /// omits the flag, falling back to the CLI's default (load everything).
    /// Evidence: `docs/SPIKE-M9.md`. Requires `claude` >= 2.1.236.
    setting_sources: Option<String>,
    /// Whether to set `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` on the spawned
    /// child. Default `true` — auto-memory is a separate mechanism from
    /// `--setting-sources` and is not affected by it (`docs/SPIKE-M9.md`
    /// §4); left on, a spawned role session can receive another
    /// session's auto-memory content. `false` omits the env var
    /// entirely (never sets it to "0" or ""). Evidence:
    /// `docs/SPIKE-M13-automemory.md`. Requires `claude` >= 2.1.278
    /// (documented at docs.claude.com/en/docs/claude-code/memory;
    /// absent from `--help`).
    disable_auto_memory: bool,
}

impl ClaudeCodeHarness {
    pub fn new() -> Self {
        Self {
            claude_bin: "claude".to_string(),
            extra_env: Vec::new(),
            setting_sources: Some(DEFAULT_SETTING_SOURCES.to_string()),
            disable_auto_memory: true,
        }
    }

    /// Point at a stand-in binary instead of the real `claude` — used by
    /// the fake-CLI-driven test suite (D7).
    pub fn with_binary(claude_bin: impl Into<String>) -> Self {
        Self {
            claude_bin: claude_bin.into(),
            extra_env: Vec::new(),
            setting_sources: Some(DEFAULT_SETTING_SOURCES.to_string()),
            disable_auto_memory: true,
        }
    }

    /// Set an extra env var on the spawned child, scoped to this harness
    /// instance (not the test process) so parallel tests selecting
    /// different `FAKE_MODE`s via env don't race each other.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    /// Override the spawned session's `--setting-sources` value. `None`
    /// omits the flag entirely rather than passing an empty value.
    pub fn with_setting_sources(mut self, sources: Option<impl Into<String>>) -> Self {
        self.setting_sources = sources.map(Into::into);
        self
    }

    /// Toggle the `CLAUDE_CODE_DISABLE_AUTO_MEMORY` env var on the
    /// spawned child. `false` omits the env var entirely rather than
    /// passing an empty or "0" value (D3).
    pub fn with_auto_memory_disabled(mut self, disabled: bool) -> Self {
        self.disable_auto_memory = disabled;
        self
    }

    /// `["--setting-sources", "<value>"]` if set, else empty.
    fn setting_sources_args(&self) -> Vec<String> {
        match &self.setting_sources {
            Some(value) => vec!["--setting-sources".to_string(), value.clone()],
            None => Vec::new(),
        }
    }

    /// `[(CLAUDE_CODE_DISABLE_AUTO_MEMORY, "1")]` if enabled, else empty.
    fn auto_memory_env(&self) -> Vec<(String, String)> {
        if self.disable_auto_memory {
            vec![(
                AUTO_MEMORY_DISABLE_ENV.to_string(),
                AUTO_MEMORY_DISABLE_VALUE.to_string(),
            )]
        } else {
            Vec::new()
        }
    }

    /// Resume a previous session (D8). Real-CLI-only: fake CLI has no
    /// persisted session state to resume.
    pub async fn spawn_resumed(
        &self,
        cfg: &AgentCfg,
        session_id: Uuid,
    ) -> Result<Session, HarnessError> {
        let mut args = vec![
            "-p".to_string(),
            "-r".to_string(),
            session_id.to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];
        args.extend(self.setting_sources_args());
        self.spawn_with_args(cfg, &args, session_id).await
    }

    async fn spawn_with_args(
        &self,
        cfg: &AgentCfg,
        args: &[String],
        session_id: Uuid,
    ) -> Result<Session, HarnessError> {
        let auto_memory_env = self.auto_memory_env();
        let mut command = Command::new(&self.claude_bin);
        command
            .args(args)
            .current_dir(&cfg.cwd)
            .envs(self.extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .envs(auto_memory_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(HarnessError::Spawn)?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let (events_tx, events_rx) = mpsc::channel(EVENTS_CHANNEL_CAPACITY);
        let (turn_tx, turn_rx) = mpsc::channel(TURN_CHANNEL_CAPACITY);
        let reported_session_id: Arc<OnceLock<String>> = Arc::new(OnceLock::new());
        let reader_task = tokio::spawn(read_events(
            stdout,
            events_tx,
            turn_tx,
            reported_session_id.clone(),
        ));
        let stderr_task = tokio::spawn(drain_stderr(stderr));

        Ok(Session {
            session_id,
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            events_rx: Some(events_rx),
            turn_rx,
            reader_task,
            stderr_task,
            reported_session_id,
        })
    }
}

impl Default for ClaudeCodeHarness {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Harness for ClaudeCodeHarness {
    fn id(&self) -> HarnessId {
        HARNESS_ID
    }

    async fn spawn(&self, cfg: &AgentCfg) -> Result<Session, HarnessError> {
        let session_id = Uuid::new_v4();
        let mut args = vec![
            "-p".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--session-id".to_string(),
            session_id.to_string(),
        ];
        args.extend(self.setting_sources_args());
        self.spawn_with_args(cfg, &args, session_id).await
    }

    async fn send(
        &self,
        session: &mut Session,
        turn: UserTurn,
        timeout: Duration,
    ) -> Result<TurnOutcome, HarnessError> {
        let line = serde_json::to_string(&json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": turn.text}],
            }
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

    async fn shutdown(&self, mut session: Session) -> Result<(), HarnessError> {
        session.reader_task.abort();
        session.stderr_task.abort();
        let _ = session.child.start_kill();
        let _ = session.child.wait().await;
        Ok(())
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
}

/// Reads NDJSON lines from `stdout`, normalizes each into zero or more
/// [`HarnessEvent`]s (D2), and for every `type:"result"` line also judges
/// the turn outcome (D4) and delivers it on `turn_tx`. Also records the
/// CLI-reported session id from the first `Started` event into
/// `reported_session_id` (M5 D3) — `set` failing (already set) is ignored,
/// since only the first report matters.
async fn read_events(
    stdout: ChildStdout,
    events_tx: mpsc::Sender<HarnessEvent>,
    turn_tx: mpsc::Sender<TurnOutcome>,
    reported_session_id: Arc<OnceLock<String>>,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!("crew-harness: reading claude stdout failed: {err}");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("crew-harness: skipping unparsable event line: {err}");
                continue;
            }
        };
        let is_result = value.get("type").and_then(Value::as_str) == Some("result");

        for event in normalize(&value) {
            if let HarnessEvent::Started { session_id } = &event {
                if !session_id.is_empty() {
                    let _ = reported_session_id.set(session_id.clone());
                }
            }
            if events_tx.send(event).await.is_err() {
                return; // no one is listening anymore
            }
        }

        if is_result {
            let outcome = judge_result(&value);
            let _ = turn_tx.send(outcome).await;
        }
    }
}

/// Drains stderr into `tracing::warn` so the pipe never fills and deadlocks
/// the child (D10). `pub(crate)` so `pi.rs` reuses it verbatim (contracts-m8.md
/// §F3 D1 — same spawn/drain convention, no per-adapter reimplementation).
pub(crate) async fn drain_stderr(stderr: ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::warn!("crew-harness: claude stderr: {line}");
    }
}

/// type:"..." -> HarnessEvent(s). Unknown types are preserved as `Raw`
/// rather than dropped (D2).
fn normalize(value: &Value) -> Vec<HarnessEvent> {
    match value.get("type").and_then(Value::as_str) {
        Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            let session_id = value
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            vec![HarnessEvent::Started { session_id }]
        }
        Some("assistant") => normalize_assistant(value),
        Some("result") => vec![match judge_result(value) {
            TurnOutcome::Success => HarnessEvent::Finished {
                ok: true,
                reason: None,
            },
            TurnOutcome::Failed { error } => HarnessEvent::Failed { error },
        }],
        _ => vec![HarnessEvent::Raw(value.clone())],
    }
}

fn normalize_assistant(value: &Value) -> Vec<HarnessEvent> {
    let message = value.get("message");
    let mut events = Vec::new();

    if let Some(usage) = message.and_then(|m| m.get("usage")) {
        events.push(HarnessEvent::Usage {
            input_tokens: usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
            output_tokens: usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        });
    }

    let blocks: Vec<Value> = message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => events.push(HarnessEvent::Text {
                delta: block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            Some("thinking") => events.push(HarnessEvent::Thinking),
            Some("tool_use") => events.push(HarnessEvent::ToolUse {
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            _ => events.push(HarnessEvent::Raw(block)),
        }
    }

    if events.is_empty() {
        events.push(HarnessEvent::Raw(value.clone()));
    }
    events
}
