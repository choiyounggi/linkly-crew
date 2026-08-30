// Global search widget (contracts-m7.md §E11): submit calls
// `source.searchMessages` when present (Tauri = FTS), else falls back to a
// client-side filter over `store.messages` (D1/D2). `source` is injectable
// (plan D2, same convention as RosterPanel) so tests don't need a global
// mock; defaults to the app's `defaultSource`.

import { useRef, useState, type FormEvent } from "react";

import { defaultSource, useRunStore } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { Envelope } from "../../lib/types";
import { fallbackSearch, snippet } from "./derive";
import "./search.css";

interface SearchBoxProps {
  source?: RunEventSource;
}

type Status = "idle" | "loading" | "error";

export default function SearchBox({ source = defaultSource }: SearchBoxProps) {
  const messages = useRunStore((s) => s.messages);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<{ seq: number; envelope: Envelope }[]>([]);
  const [status, setStatus] = useState<Status>("idle");
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  // D3: only the latest submission's response is applied — discards stale
  // resolutions from an earlier, slower in-flight search.
  const submissionRef = useRef(0);

  const close = () => setOpen(false);

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    const trimmed = query.trim();
    if (!trimmed) return;

    const submissionId = ++submissionRef.current;
    setOpen(true);
    setStatus("loading");
    setError(null);

    const run = async () => {
      try {
        const found = source.searchMessages ? await source.searchMessages(trimmed) : fallbackSearch(messages, trimmed);
        if (submissionRef.current !== submissionId) return;
        setResults(found);
        setStatus("idle");
      } catch (err) {
        if (submissionRef.current !== submissionId) return;
        setError(err instanceof Error ? err.message : String(err));
        setStatus("error");
      }
    };
    void run();
  };

  return (
    <div className="search-box">
      <form onSubmit={handleSubmit}>
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="검색"
          aria-label="전역 검색"
        />
        <button type="submit">검색</button>
      </form>
      {open && (
        <div className="search-box__panel" role="listbox" aria-label="검색 결과">
          <button type="button" className="search-box__close" aria-label="닫기" onClick={close}>
            ×
          </button>
          {status === "loading" && <p className="search-box__status">검색 중…</p>}
          {status === "error" && <p className="search-box__status search-box__status--error">{error}</p>}
          {status === "idle" && results.length === 0 && <p className="search-box__status">결과 없음</p>}
          {status === "idle" && results.length > 0 && (
            <ul className="search-box__results">
              {results.map(({ seq, envelope }) => (
                <li key={seq} className="search-box__result">
                  <span className="search-box__result-kind">{envelope.kind}</span>
                  <span className="search-box__result-from">{envelope.from}</span>
                  <span className="search-box__result-ts">{envelope.ts}</span>
                  <p className="search-box__result-snippet">{snippet(envelope)}</p>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
