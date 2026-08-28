import { useRunStore } from "../../lib/store";

export default function Rail() {
  const roleCount = useRunStore((s) => s.sprint.length);
  return (
    <section className="panel panel--rail" aria-label="에이전트 레일">
      <h2>에이전트 레일</h2>
      <p>곧 제공됩니다 (역할 {roleCount}개)</p>
    </section>
  );
}
