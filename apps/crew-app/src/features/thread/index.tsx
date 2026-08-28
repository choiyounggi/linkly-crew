import { useRunStore } from "../../lib/store";

export default function Thread() {
  const messageCount = useRunStore((s) => s.messages.length);
  return (
    <section className="panel panel--thread" aria-label="라이브 스레드">
      <h2>라이브 스레드</h2>
      <p>곧 제공됩니다 (메시지 {messageCount}개)</p>
    </section>
  );
}
