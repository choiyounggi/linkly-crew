import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { TerminalPanelImpl } from "./index";
import type { Invoke, Listen } from "./index";

// xterm.js needs a real browser canvas + matchMedia to actually render,
// both unavailable under jsdom. This component's own contract — opening a
// pty, wiring output/input, injectText, cleanup — doesn't depend on xterm's
// internal rendering, so its public surface is faked here instead.
const writes: string[] = [];
const disposeCalls: number[] = [];
let lastOnDataHandler: ((data: string) => void) | null = null;
let lastResizeObserverCallback: (() => void) | null = null;

vi.mock("@xterm/xterm", () => ({
  Terminal: vi.fn().mockImplementation(() => ({
    cols: 80,
    rows: 24,
    loadAddon: vi.fn(),
    open: vi.fn(),
    write: vi.fn((chunk: string) => writes.push(chunk)),
    onData: vi.fn((handler: (data: string) => void) => {
      lastOnDataHandler = handler;
    }),
    dispose: vi.fn(() => disposeCalls.push(1)),
  })),
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: vi.fn().mockImplementation(() => ({ fit: vi.fn() })),
}));

// D10-style stub (features/dag/index.test.tsx): ResizeObserver is absent in
// jsdom. Unlike that stub, this one always installs (not only when absent)
// and captures the callback so tests can trigger a simulated resize.
beforeAll(() => {
  class ResizeObserverStub {
    constructor(cb: () => void) {
      lastResizeObserverCallback = cb;
    }
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).ResizeObserver = ResizeObserverStub;
});

afterEach(() => {
  cleanup();
  writes.length = 0;
  disposeCalls.length = 0;
  lastOnDataHandler = null;
  lastResizeObserverCallback = null;
  vi.clearAllMocks();
});

/** A fake `listen` that hands the test its handler so it can push output on demand. */
function fakeListen(): { listen: Listen; push: (id: number, chunk: string) => void; unlisten: ReturnType<typeof vi.fn> } {
  const handlers = new Map<string, (payload: string) => void>();
  const unlisten = vi.fn();
  const listen: Listen = async (event, handler) => {
    handlers.set(event, handler);
    return unlisten;
  };
  return {
    listen,
    push: (id, chunk) => handlers.get(`pty://output/${id}`)?.(chunk),
    unlisten,
  };
}

function fakeInvoke(openId = 1): Invoke {
  return vi.fn(async (cmd: string) => {
    if (cmd === "pty_open") return openId as never;
    return undefined as never;
  });
}

describe("TerminalPanel", () => {
  it("opens a pty on mount and writes pty://output events into the terminal (normal)", async () => {
    const invoke = fakeInvoke(1);
    const { listen, push } = fakeListen();

    await act(async () => {
      render(<TerminalPanelImpl invoke={invoke} listen={listen} />);
    });

    expect(invoke).toHaveBeenCalledWith("pty_open");
    await act(async () => {
      push(1, "hello from shell\r\n");
    });
    expect(writes).toContain("hello from shell\r\n");
  });

  it("wires keystrokes typed in the terminal straight through to pty_write (bidirectional IO, input direction)", async () => {
    const invoke = fakeInvoke(1);
    const { listen } = fakeListen();

    await act(async () => {
      render(<TerminalPanelImpl invoke={invoke} listen={listen} />);
    });

    expect(lastOnDataHandler).not.toBeNull();
    await act(async () => {
      lastOnDataHandler?.("ls -la\n");
    });

    expect(invoke).toHaveBeenCalledWith("pty_write", { id: 1, data: "ls -la\n" });
  });

  it("resizes the pty to fit on mount and again whenever the container's ResizeObserver fires", async () => {
    const invoke = fakeInvoke(1);
    const { listen } = fakeListen();

    await act(async () => {
      render(<TerminalPanelImpl invoke={invoke} listen={listen} />);
    });

    // mocked Terminal reports cols=80/rows=24 (see the @xterm/xterm mock above)
    expect(invoke).toHaveBeenCalledWith("pty_resize", { id: 1, cols: 80, rows: 24 });
    const resizeCallsBefore = (invoke as ReturnType<typeof vi.fn>).mock.calls.filter(([cmd]) => cmd === "pty_resize").length;

    expect(lastResizeObserverCallback).not.toBeNull();
    await act(async () => {
      lastResizeObserverCallback?.();
    });

    const resizeCallsAfter = (invoke as ReturnType<typeof vi.fn>).mock.calls.filter(([cmd]) => cmd === "pty_resize").length;
    expect(resizeCallsAfter).toBeGreaterThan(resizeCallsBefore);
  });

  it("forwards injectText to pty_write verbatim, with no trailing newline, and calls onInjected exactly once (boundary, plan D5)", async () => {
    const invoke = fakeInvoke(1);
    const { listen } = fakeListen();
    const onInjected = vi.fn();

    const { rerender } = render(<TerminalPanelImpl invoke={invoke} listen={listen} injectText={null} onInjected={onInjected} />);
    await act(async () => {});

    await act(async () => {
      rerender(<TerminalPanelImpl invoke={invoke} listen={listen} injectText="npm install" onInjected={onInjected} />);
    });

    expect(invoke).toHaveBeenCalledWith("pty_write", { id: 1, data: "npm install" });
    expect(onInjected).toHaveBeenCalledTimes(1);

    const writeCalls = (invoke as ReturnType<typeof vi.fn>).mock.calls.filter(([cmd]) => cmd === "pty_write");
    expect(writeCalls.length).toBeGreaterThan(0);
    for (const [, args] of writeCalls) {
      expect((args as { data: string }).data).not.toContain("\n");
    }
  });

  it("closes the pty and disposes the terminal on unmount (cleanup)", async () => {
    const invoke = fakeInvoke(1);
    const { listen, unlisten } = fakeListen();

    const { unmount } = render(<TerminalPanelImpl invoke={invoke} listen={listen} />);
    await act(async () => {});

    await act(async () => {
      unmount();
    });

    expect(invoke).toHaveBeenCalledWith("pty_close", { id: 1 });
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(disposeCalls).toHaveLength(1);
  });

  it("closes the pty it opened even when unmounted before pty_open resolves (error/race path)", async () => {
    let resolveOpen!: (id: number) => void;
    const opened = new Promise<number>((resolve) => {
      resolveOpen = resolve;
    });
    const invoke: Invoke = vi.fn((cmd: string) => {
      if (cmd === "pty_open") return opened as never;
      return Promise.resolve(undefined) as never;
    });
    const { listen } = fakeListen();

    const { unmount } = render(<TerminalPanelImpl invoke={invoke} listen={listen} />);
    unmount();

    await act(async () => {
      resolveOpen(1);
      await opened;
    });

    expect(invoke).toHaveBeenCalledWith("pty_close", { id: 1 });
  });
});
