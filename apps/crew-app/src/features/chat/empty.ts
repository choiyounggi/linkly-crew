// Stable empty fallbacks for useRunStore selectors. A fresh `[]`/`{}`
// literal per selector call (`s.channels[runId]?.messages ?? []`) gives
// useSyncExternalStore a "changed" snapshot on every render and
// infinite-loops (verified: ChatStream.test.tsx's no-channel boundary case
// triggered "Maximum update depth exceeded" before this fix). Every selector
// with a missing-channel fallback must use one of these instead of a literal.

import type { ChannelState } from "../../lib/store";

export const EMPTY_MESSAGES: ChannelState["messages"] = [];
export const EMPTY_READ_RECEIPTS: ChannelState["readReceipts"] = {};
export const EMPTY_TYPING: ChannelState["typing"] = {};
export const EMPTY_READERS: string[] = [];
