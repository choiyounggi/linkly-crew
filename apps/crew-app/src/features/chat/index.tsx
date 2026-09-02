// contract: t7-fe-chat owns the implementation
// Stable surface consumed by t6-fe-shell: the channel's center chat pane.
export interface ChatPaneProps {
  runId: string;
}

export default function ChatPane(_props: ChatPaneProps) {
  return <section className="chat-pane" aria-label="채널 대화" />;
}
