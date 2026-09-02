//! Embedded terminal PTY backend (t5-terminal brief): `pty_open` /
//! `pty_write` / `pty_resize` / `pty_close` commands over `portable-pty`,
//! streamed on `pty://output/<id>` / `pty://exit/<id>` — a namespace kept
//! separate from `lib.rs`'s `"run://event"` pump (out of scope here).
//!
//! Tauri-independent core (`open_session`/`write_session`/`resize_session`/
//! `close_session`) mirrors `core.rs`'s `emit: F` pattern: tests inject
//! plain closures instead of a real `AppHandle`, so the command wrappers at
//! the bottom of this file stay thin.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tauri::{AppHandle, Emitter, State};

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

struct PtySession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
}

/// `tauri::State`-managed registry (plan D4): `Mutex<HashMap<id, session>>`,
/// one entry per open PTY, id-routed so concurrent sessions never
/// interfere (R6). Ids are app-assigned (never the OS pid) so a natural
/// child exit and pty_close both key off the same stable handle.
#[derive(Default, Clone)]
pub struct PtyRegistry(Arc<Mutex<HashMap<u32, PtySession>>>);

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Opens a PTY session running `shell`, spawns a dedicated OS thread to pump
/// its output (plan D6: portable-pty's reader is blocking `std::io::Read`;
/// pumping it inside the tokio runtime would block the executor), and
/// returns the new session's id. `on_output` fires per chunk read;
/// `on_exit` fires once, after the child is reaped, with its exit code —
/// then the session is removed from `registry` (plan D6: no orphaned
/// entries survive a natural child exit).
pub fn open_session<O, X>(registry: &PtyRegistry, shell: &str, on_output: O, on_exit: X) -> Result<u32, String>
where
    O: Fn(u32, String) + Send + 'static,
    X: Fn(u32, u32) + Send + 'static,
{
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize { rows: DEFAULT_ROWS, cols: DEFAULT_COLS, pixel_width: 0, pixel_height: 0 })
        .map_err(|err| format!("openpty failed: {err}"))?;

    let cmd = CommandBuilder::new(shell);
    let mut child = pair.slave.spawn_command(cmd).map_err(|err| format!("spawn_command failed: {err}"))?;
    // Drop our copy of the slave fd: on unix the master's reader only sees
    // EOF once every slave-side fd (ours plus the child's) is closed.
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().map_err(|err| format!("try_clone_reader failed: {err}"))?;
    let writer = pair.master.take_writer().map_err(|err| format!("take_writer failed: {err}"))?;
    let killer = child.clone_killer();

    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let map = registry.0.clone();
    map.lock()
        .unwrap()
        .insert(id, PtySession { writer, master: pair.master, killer, reader_thread: None });

    let map_for_thread = map.clone();
    let handle = std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => on_output(id, String::from_utf8_lossy(&buf[..n]).into_owned()),
                Err(_) => break,
            }
        }
        let code = child.wait().map(|status| status.exit_code()).unwrap_or(1);
        on_exit(id, code);
        map_for_thread.lock().unwrap().remove(&id);
    });

    if let Some(session) = map.lock().unwrap().get_mut(&id) {
        session.reader_thread = Some(handle);
    }

    Ok(id)
}

/// Writes `data` to session `id` verbatim (plan D5: own-shell passthrough —
/// the PTY runs the user's own login shell, not a privileged process, so
/// there is no trust boundary for this app to enforce beyond "never inject
/// a newline the user didn't type"). No `\n`/execution is appended: a
/// pasted install command sits in the shell's input line until the user
/// presses Enter themselves.
pub fn write_session(registry: &PtyRegistry, id: u32, data: &str) -> Result<(), String> {
    let mut map = registry.0.lock().unwrap();
    let session = map.get_mut(&id).ok_or_else(|| format!("pty session {id} not found"))?;
    session.writer.write_all(data.as_bytes()).map_err(|err| err.to_string())?;
    session.writer.flush().map_err(|err| err.to_string())
}

/// Resizes session `id`'s pty (R3) — `MasterPty::resize` issues `TIOCSWINSZ`
/// so a running TUI reflows on its next read of the window size.
pub fn resize_session(registry: &PtyRegistry, id: u32, cols: u16, rows: u16) -> Result<(), String> {
    let map = registry.0.lock().unwrap();
    let session = map.get(&id).ok_or_else(|| format!("pty session {id} not found"))?;
    session
        .master
        .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|err| err.to_string())
}

/// Kills session `id`'s child and joins its reader thread before returning
/// (plan D6/task steps: kill + join + registry removal) — an unknown/
/// already-closed id is a no-op, not an error, since `TerminalPanel`'s
/// unmount and `pty://exit`'s own natural-exit cleanup can race harmlessly.
/// Registry removal itself happens inside the reader thread (see
/// `open_session`), so this only needs to wait for that thread to finish.
pub fn close_session(registry: &PtyRegistry, id: u32) -> Result<(), String> {
    let (kill_result, handle) = {
        let mut map = registry.0.lock().unwrap();
        match map.get_mut(&id) {
            Some(session) => (session.killer.kill().map_err(|err| err.to_string()), session.reader_thread.take()),
            None => return Ok(()),
        }
    };
    kill_result?;
    if let Some(handle) = handle {
        let _ = handle.join();
    }
    Ok(())
}

#[tauri::command]
pub fn pty_open(app: AppHandle, registry: State<'_, PtyRegistry>) -> Result<u32, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let app_output = app.clone();
    let app_exit = app;
    open_session(
        &registry,
        &shell,
        move |id, chunk| {
            if let Err(err) = app_output.emit(&format!("pty://output/{id}"), chunk) {
                tracing::warn!(error = %err, id, "pty://output emit failed");
            }
        },
        move |id, code| {
            if let Err(err) = app_exit.emit(&format!("pty://exit/{id}"), code) {
                tracing::warn!(error = %err, id, "pty://exit emit failed");
            }
        },
    )
}

#[tauri::command]
pub fn pty_write(id: u32, data: String, registry: State<'_, PtyRegistry>) -> Result<(), String> {
    write_session(&registry, id, &data)
}

#[tauri::command]
pub fn pty_resize(id: u32, cols: u16, rows: u16, registry: State<'_, PtyRegistry>) -> Result<(), String> {
    resize_session(&registry, id, cols, rows)
}

#[tauri::command]
pub fn pty_close(id: u32, registry: State<'_, PtyRegistry>) -> Result<(), String> {
    close_session(&registry, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver};
    use std::time::{Duration, Instant};

    const SH: &str = "/bin/sh";
    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Accumulates chunks for `id` from `rx` until the buffer contains
    /// `needle` or `TIMEOUT` elapses; returns whatever was accumulated.
    fn drain_until(rx: &Receiver<(u32, String)>, id: u32, needle: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        let mut buf = String::new();
        while !buf.contains(needle) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok((chunk_id, chunk)) if chunk_id == id => buf.push_str(&chunk),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        buf
    }

    /// Accumulates every chunk for `id` from `rx` that arrives within
    /// `duration` (unconditionally waits out the full window, unlike
    /// `drain_until`) — used to prove something did NOT happen.
    fn drain_for(rx: &Receiver<(u32, String)>, id: u32, duration: Duration) -> String {
        let deadline = Instant::now() + duration;
        let mut buf = String::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok((chunk_id, chunk)) if chunk_id == id => buf.push_str(&chunk),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        buf
    }

    /// Opens a session over `SH` wired to test channels: `on_output` sends
    /// `(id, chunk)`, `on_exit` sends `(id, code)`.
    fn open_test_session() -> (PtyRegistry, u32, Receiver<(u32, String)>, Receiver<(u32, u32)>) {
        let registry = PtyRegistry::default();
        let (out_tx, out_rx) = channel::<(u32, String)>();
        let (exit_tx, exit_rx) = channel::<(u32, u32)>();
        let id = open_session(
            &registry,
            SH,
            move |id, chunk| {
                let _ = out_tx.send((id, chunk));
            },
            move |id, code| {
                let _ = exit_tx.send((id, code));
            },
        )
        .expect("open_session");
        (registry, id, out_rx, exit_rx)
    }

    #[test]
    fn echo_roundtrips_through_the_pty(/* R1/R2, normal */) {
        let (registry, id, out_rx, _exit_rx) = open_test_session();

        write_session(&registry, id, "echo hi_from_pty\n").expect("write_session");
        let output = drain_until(&out_rx, id, "hi_from_pty");

        assert!(output.contains("hi_from_pty"), "expected echoed output, got: {output:?}");
        close_session(&registry, id).expect("close_session");
    }

    #[test]
    fn resize_updates_the_ptys_reported_window_size(/* R3, boundary */) {
        let (registry, id, out_rx, _exit_rx) = open_test_session();

        resize_session(&registry, id, 100, 40).expect("resize_session");
        write_session(&registry, id, "stty size\n").expect("write_session");
        let output = drain_until(&out_rx, id, "40 100");

        assert!(output.contains("40 100"), "expected `stty size` to report the resized window, got: {output:?}");
        close_session(&registry, id).expect("close_session");
    }

    #[test]
    fn close_kills_the_child_and_removes_the_session(/* R4, error/cleanup */) {
        let (registry, id, _out_rx, exit_rx) = open_test_session();

        close_session(&registry, id).expect("close_session");

        exit_rx.recv_timeout(TIMEOUT).expect("pty://exit fired after close");
        assert!(
            !registry.0.lock().unwrap().contains_key(&id),
            "session should be removed from the registry after close"
        );
        let write_after_close = write_session(&registry, id, "echo late\n");
        assert!(write_after_close.is_err(), "writing to a closed session must error, not silently no-op");
    }

    #[test]
    fn concurrent_sessions_do_not_leak_output_into_each_other(/* R6, boundary */) {
        let (registry_a, id_a, out_rx_a, _exit_a) = open_test_session();
        let (registry_b, id_b, out_rx_b, _exit_b) = open_test_session();
        assert_ne!(id_a, id_b, "distinct sessions must get distinct ids");

        write_session(&registry_a, id_a, "echo only_in_a\n").expect("write_session a");
        write_session(&registry_b, id_b, "echo only_in_b\n").expect("write_session b");

        let output_a = drain_until(&out_rx_a, id_a, "only_in_a");
        let output_b = drain_until(&out_rx_b, id_b, "only_in_b");

        assert!(output_a.contains("only_in_a"), "session a missing its own output: {output_a:?}");
        assert!(!output_a.contains("only_in_b"), "session a leaked session b's output: {output_a:?}");
        assert!(output_b.contains("only_in_b"), "session b missing its own output: {output_b:?}");
        assert!(!output_b.contains("only_in_a"), "session b leaked session a's output: {output_b:?}");

        close_session(&registry_a, id_a).expect("close a");
        close_session(&registry_b, id_b).expect("close b");
    }

    #[test]
    fn write_without_a_trailing_newline_never_executes(/* D5, boundary */) {
        let (registry, id, out_rx, _exit_rx) = open_test_session();

        // No trailing '\n': the pty's line discipline buffers the raw
        // keystrokes but never delivers the line to the shell, so it never
        // runs. The *typed* text ("dekovni_ton") and what its *execution*
        // would print ("not_invoked", piped through `rev`) are deliberately
        // different strings — the shell's line editor can legitimately
        // echo raw keystrokes more than once as it takes over the tty
        // (kernel cooked-mode echo, then its own raw-mode redraw), so
        // asserting an exact echo count would be flaky. Asserting the
        // execution-only string never appears is not: it can only exist if
        // the pipeline actually ran.
        write_session(&registry, id, "echo dekovni_ton | rev").expect("write_session");
        let output = drain_for(&out_rx, id, Duration::from_millis(500));

        assert!(output.contains("dekovni_ton"), "expected the unsent line to still be echoed, got: {output:?}");
        assert!(
            !output.contains("not_invoked"),
            "unsent line must not execute (no auto-Enter, plan D5), got: {output:?}"
        );
        close_session(&registry, id).expect("close_session");
    }
}
