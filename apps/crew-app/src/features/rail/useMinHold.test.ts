import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentCard } from "./derive";
import { useMinHoldCards } from "./useMinHold";

function card(overrides: Partial<AgentCard>): AgentCard {
  return {
    id: "lead",
    role: "lead",
    status: "idle",
    currentTaskId: null,
    harness: "claude-code",
    ...overrides,
  };
}

describe("useMinHoldCards", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("holds a transient status (awaiting) for holdMs before dropping to the latest status", () => {
    const { result, rerender } = renderHook(({ cards }) => useMinHoldCards(cards), {
      initialProps: { cards: [card({ status: "awaiting" })] },
    });

    expect(result.current[0].status).toBe("awaiting");

    rerender({ cards: [card({ status: "idle" })] });
    expect(result.current[0].status).toBe("awaiting");

    act(() => {
      vi.advanceTimersByTime(599);
    });
    expect(result.current[0].status).toBe("awaiting");

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current[0].status).toBe("idle");
  });

  it("coalesces multiple status changes during a hold into the latest status at expiry", () => {
    const { result, rerender } = renderHook(({ cards }) => useMinHoldCards(cards), {
      initialProps: { cards: [card({ status: "awaiting" })] },
    });

    rerender({ cards: [card({ status: "working" })] });
    act(() => {
      vi.advanceTimersByTime(300);
    });
    rerender({ cards: [card({ status: "idle" })] });
    act(() => {
      vi.advanceTimersByTime(299);
    });

    expect(result.current[0].status).toBe("awaiting");

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current[0].status).toBe("idle");
  });

  it("starts a new chained hold if the status is transient again right at expiry", () => {
    const { result, rerender } = renderHook(({ cards }) => useMinHoldCards(cards), {
      initialProps: { cards: [card({ status: "awaiting" })] },
    });

    act(() => {
      vi.advanceTimersByTime(600);
    });
    expect(result.current[0].status).toBe("awaiting");

    rerender({ cards: [card({ status: "working" })] });
    expect(result.current[0].status).toBe("awaiting");

    act(() => {
      vi.advanceTimersByTime(599);
    });
    expect(result.current[0].status).toBe("awaiting");

    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(result.current[0].status).toBe("working");
  });

  it("passes through an empty cards array without error", () => {
    const { result } = renderHook(({ cards }) => useMinHoldCards(cards), {
      initialProps: { cards: [] as AgentCard[] },
    });

    expect(result.current).toEqual([]);
  });

  it("clears pending timers on unmount without leaking (no post-unmount state updates)", () => {
    const clearSpy = vi.spyOn(globalThis, "clearTimeout");
    const { unmount } = renderHook(({ cards }) => useMinHoldCards(cards), {
      initialProps: { cards: [card({ status: "awaiting" })] },
    });

    unmount();

    expect(clearSpy).toHaveBeenCalled();
    act(() => {
      vi.advanceTimersByTime(1000);
    });

    clearSpy.mockRestore();
  });

  it("does not hold idle status — once resolved to idle, the next transient change starts its own fresh hold", () => {
    const { result, rerender } = renderHook(({ cards }) => useMinHoldCards(cards), {
      initialProps: { cards: [card({ status: "awaiting" })] },
    });

    rerender({ cards: [card({ status: "idle" })] });
    act(() => {
      vi.advanceTimersByTime(600);
    });
    expect(result.current[0].status).toBe("idle");

    rerender({ cards: [card({ status: "working" })] });
    expect(result.current[0].status).toBe("working");
  });

  it("passes through immediately when holdMs=0", () => {
    const { result, rerender } = renderHook(({ cards, holdMs }) => useMinHoldCards(cards, holdMs), {
      initialProps: { cards: [card({ status: "awaiting" })], holdMs: 0 },
    });

    expect(result.current[0].status).toBe("awaiting");

    rerender({ cards: [card({ status: "idle" })], holdMs: 0 });
    expect(result.current[0].status).toBe("idle");
  });

  it("passes through non-status fields (currentTaskId, harness) as the latest value, unheld", () => {
    const { result, rerender } = renderHook(({ cards }) => useMinHoldCards(cards), {
      initialProps: { cards: [card({ status: "awaiting", currentTaskId: "t-1" })] },
    });

    rerender({ cards: [card({ status: "idle", currentTaskId: "t-2" })] });

    expect(result.current[0].status).toBe("awaiting");
    expect(result.current[0].currentTaskId).toBe("t-2");
  });
});
