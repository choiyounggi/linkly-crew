import type { Envelope } from "../../lib/types";

const SNIPPET_LENGTH = 120;

/**
 * Client-side fallback filter (contracts-m7.md §E11) used when
 * `source.searchMessages` is absent. Matches the same target the
 * `MockEventSource.searchMessages` implementation searches over
 * (kind + from + JSON.stringify(body)), so results are consistent
 * whether the FTS backend or this fallback served them.
 */
export function fallbackSearch(
  messages: { seq: number; envelope: Envelope }[],
  query: string,
): { seq: number; envelope: Envelope }[] {
  const needle = query.toLowerCase();
  return messages.filter(({ envelope }) => {
    const haystack = `${envelope.kind} ${envelope.from} ${JSON.stringify(envelope.body ?? "")}`.toLowerCase();
    return haystack.includes(needle);
  });
}

/** Body text for display: the raw string when body is already a string, else its JSON form. */
function bodyText(envelope: Envelope): string {
  return typeof envelope.body === "string" ? envelope.body : JSON.stringify(envelope.body ?? "");
}

/** Truncates a result's body to `SNIPPET_LENGTH` characters, appending "…" when cut. */
export function snippet(envelope: Envelope): string {
  const text = bodyText(envelope);
  return text.length > SNIPPET_LENGTH ? `${text.slice(0, SNIPPET_LENGTH)}…` : text;
}
