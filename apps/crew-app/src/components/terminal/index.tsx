// contract: t5-terminal owns the implementation
// Stable surface consumed by t8-fe-onboarding: <TerminalPanel /> mounts an
// embedded PTY terminal; `injectText` types text into the shell WITHOUT
// executing it (the user presses Enter).
import { useEffect, useRef, useState } from "react";

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import "./terminal.css";

export interface TerminalPanelProps {
  /** Text to type into the terminal (no auto-Enter). Cleared by onInjected. */
  injectText?: string | null;
  onInjected?: () => void;
}

/** `invoke`'s shape, narrowed to what this component calls — injectable for tests (mirrors `lib/tauri-source.ts`'s `Invoke`). */
export type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
/** `listen`'s shape, narrowed to a raw string payload (`pty://output/<id>`'s chunk) — injectable for tests. */
export type Listen = (event: string, handler: (payload: string) => void) => Promise<UnlistenFn>;

function defaultInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return tauriInvoke<T>(cmd, args);
}

function defaultListen(event: string, handler: (payload: string) => void): Promise<UnlistenFn> {
  return tauriListen<string>(event, (e) => handler(e.payload));
}

interface TerminalPanelImplProps extends TerminalPanelProps {
  invoke?: Invoke;
  listen?: Listen;
}

/**
 * Injectable implementation (plan D2/D3): tests pass fake `invoke`/`listen`
 * instead of a real Tauri bridge (unavailable under vitest/jsdom). The
 * default export below wraps this with the real bridge and keeps
 * `TerminalPanelProps`'s locked signature — t8-fe-onboarding depends on it.
 */
export function TerminalPanelImpl({ injectText, onInjected, invoke = defaultInvoke, listen = defaultListen }: TerminalPanelImplProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  // Drives the injectText effect once pty_open resolves (a plain ref
  // wouldn't re-trigger that effect if injectText was already set before
  // the pty was ready — rerender-and-memoization.md: state only where a
  // later effect must react to the value becoming available).
  const [readyId, setReadyId] = useState<number | null>(null);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    let disposed = false;
    let openedId: number | null = null;
    let unlistenOutput: UnlistenFn | null = null;
    let resizeObserver: ResizeObserver | null = null;

    // One xterm instance for this mount (D2: never recreated on rerender —
    // this effect's deps are `[invoke, listen]`, both stable by default).
    const term = new Terminal({ convertEol: true, cursorBlink: true });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(container);

    const resizeToFit = (id: number) => {
      fitAddon.fit();
      void invoke("pty_resize", { id, cols: term.cols, rows: term.rows });
    };

    (async () => {
      let id: number;
      try {
        id = await invoke<number>("pty_open");
      } catch (err) {
        console.error("pty_open failed", err);
        return;
      }
      if (disposed) {
        // Unmounted before pty_open resolved — nothing was wired up yet,
        // just close what we opened.
        void invoke("pty_close", { id });
        return;
      }
      openedId = id;

      unlistenOutput = await listen(`pty://output/${id}`, (chunk) => {
        term.write(chunk);
      });

      // D5: raw passthrough — every keystroke goes straight to the pty,
      // no interpretation or auto-Enter on this app's side.
      term.onData((data) => {
        void invoke("pty_write", { id, data });
      });

      resizeToFit(id);
      resizeObserver = new ResizeObserver(() => resizeToFit(id));
      resizeObserver.observe(container);

      setReadyId(id);
    })();

    return () => {
      disposed = true;
      resizeObserver?.disconnect();
      void unlistenOutput?.();
      if (openedId !== null) {
        void invoke("pty_close", { id: openedId });
      }
      term.dispose();
    };
  }, [invoke, listen]);

  useEffect(() => {
    if (injectText == null || readyId === null) return;
    // D5: text only, never a trailing '\n' — running it is the user's own
    // Enter keypress, not this component's job.
    void invoke("pty_write", { id: readyId, data: injectText }).then(() => {
      onInjected?.();
    });
  }, [injectText, readyId, invoke, onInjected]);

  return <div ref={containerRef} className="terminal-panel" aria-label="터미널" />;
}

export default function TerminalPanel(props: TerminalPanelProps) {
  return <TerminalPanelImpl {...props} />;
}
