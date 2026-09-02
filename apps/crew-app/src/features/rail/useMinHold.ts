import { useEffect, useRef, useState } from "react";

import type { AgentCard, RailStatus } from "./derive";

export const MIN_HOLD_MS = 600;

function isTransient(status: RailStatus): boolean {
  return status === "working" || status === "awaiting";
}

/**
 * D4: only `status` is held — currentTaskId/harness/etc always reflect the latest
 * derived card, since holding those too would show a stale task id.
 */
export function useMinHoldCards(cards: AgentCard[], holdMs: number = MIN_HOLD_MS): AgentCard[] {
  const [displayStatus, setDisplayStatus] = useState<Record<string, RailStatus>>(() =>
    Object.fromEntries(cards.map((c) => [c.id, c.status])),
  );
  const timersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  const cardsRef = useRef<AgentCard[]>(cards);
  cardsRef.current = cards;

  useEffect(() => {
    const timers = timersRef.current;

    if (holdMs <= 0) {
      for (const timer of timers.values()) clearTimeout(timer);
      timers.clear();
      return;
    }

    const expire = (id: string) => {
      timers.delete(id);
      const latest = cardsRef.current.find((c) => c.id === id);
      if (!latest) {
        setDisplayStatus((prev) => {
          if (!(id in prev)) return prev;
          const next = { ...prev };
          delete next[id];
          return next;
        });
        return;
      }
      setDisplayStatus((prev) => ({ ...prev, [id]: latest.status }));
      if (isTransient(latest.status)) {
        timers.set(
          id,
          setTimeout(() => expire(id), holdMs),
        );
      }
    };

    setDisplayStatus((prev) => {
      let changed = false;
      const next = { ...prev };

      for (const c of cards) {
        if (timers.has(c.id)) continue; // hold in progress — coalesce, ignore this update

        if (next[c.id] !== c.status) {
          next[c.id] = c.status;
          changed = true;
        }
        if (isTransient(c.status)) {
          timers.set(
            c.id,
            setTimeout(() => expire(c.id), holdMs),
          );
        }
      }

      const liveIds = new Set(cards.map((c) => c.id));
      for (const id of Object.keys(next)) {
        if (!liveIds.has(id)) {
          const timer = timers.get(id);
          if (timer) {
            clearTimeout(timer);
            timers.delete(id);
          }
          delete next[id];
          changed = true;
        }
      }

      return changed ? next : prev;
    });
  }, [cards, holdMs]);

  useEffect(() => {
    const timers = timersRef.current;
    return () => {
      for (const timer of timers.values()) clearTimeout(timer);
      timers.clear();
    };
  }, []);

  return cards.map((c) => {
    if (holdMs <= 0) return c;
    const held = displayStatus[c.id];
    return held === undefined ? c : { ...c, status: held };
  });
}
