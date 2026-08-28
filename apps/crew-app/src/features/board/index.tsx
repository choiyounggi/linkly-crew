import { useRunStore } from "../../lib/store";

export default function Board() {
  const taskCount = useRunStore((s) => s.dag?.tasks.length ?? 0);
  return (
    <section className="panel panel--board" aria-label="스프린트 보드">
      <h2>스프린트 보드</h2>
      <p>곧 제공됩니다 (태스크 {taskCount}개)</p>
    </section>
  );
}
