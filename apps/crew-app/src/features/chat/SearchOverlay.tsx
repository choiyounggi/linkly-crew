// Channel search overlay (t7 plan D9): searchMessages(runId, query) on every
// keystroke, with an ignore-stale guard (wiki/frontend/data-fetching/
// race-conditions.md's "record a token per request" mechanism) so a slow
// earlier response can never overwrite a faster later one — only the
// latest-issued query's result is ever applied.

import { useRef, useState } from "react";

import { defaultSource } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { Envelope } from "../../lib/types";

interface SearchOverlayProps {
  runId: string;
  onClose: () => void;
  onOpenThread: (threadId: string) => void;
  source?: RunEventSource;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export default function SearchOverlay({ runId, onClose, onOpenThread, source = defaultSource }: SearchOverlayProps) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<{ seq: number; envelope: Envelope }[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const latestQueryRef = useRef("");

  async function runSearch(q: string) {
    setQuery(q);
    latestQueryRef.current = q;
    if (!q) {
      setResults([]);
      setError(null);
      setLoading(false);
      return;
    }
    if (typeof source.searchMessages !== "function") return;

    setLoading(true);
    setError(null);
    try {
      const found = await source.searchMessages(runId, q);
      if (latestQueryRef.current !== q) return; // superseded by a later query — discard
      setResults(found);
    } catch (err) {
      if (latestQueryRef.current !== q) return;
      setError(errorMessage(err));
    } finally {
      if (latestQueryRef.current === q) setLoading(false);
    }
  }

  return (
    <div className="search-overlay" role="dialog" aria-label="채널 검색">
      <div className="search-overlay__bar">
        <input
          type="text"
          className="search-overlay__input"
          value={query}
          onChange={(e) => void runSearch(e.target.value)}
          placeholder="메시지 검색"
          aria-label="메시지 검색"
        />
        <button type="button" className="search-overlay__close" onClick={onClose} aria-label="검색 닫기">
          ×
        </button>
      </div>
      {loading && <p className="search-overlay__status">검색 중...</p>}
      {error && (
        <p className="search-overlay__status" role="alert">
          {error}
        </p>
      )}
      {!loading && !error && query && results.length === 0 && (
        <p className="search-overlay__status">검색 결과가 없습니다</p>
      )}
      {results.length > 0 && (
        <ul className="search-overlay__results">
          {results.map(({ envelope }) => (
            <li key={envelope.id}>
              <button type="button" onClick={() => onOpenThread(envelope.thread)}>
                <span className="search-overlay__result-from">{envelope.from}</span>
                <span className="search-overlay__result-kind">{envelope.kind}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
