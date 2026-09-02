// contract: t5-terminal owns the implementation
// Stable surface consumed by t8-fe-onboarding: <TerminalPanel /> mounts an
// embedded PTY terminal; `injectText` types text into the shell WITHOUT
// executing it (the user presses Enter).
export interface TerminalPanelProps {
  /** Text to type into the terminal (no auto-Enter). Cleared by onInjected. */
  injectText?: string | null;
  onInjected?: () => void;
}

export default function TerminalPanel(_props: TerminalPanelProps) {
  return <div className="terminal-panel" aria-label="터미널" />;
}
