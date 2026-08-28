// Stub only — t-ui-core owns this mount point, t-ui-roster owns the real
// panel content (contracts-m5.md §C7a/§C7b).

export default function RosterPanel() {
  return (
    <section className="panel panel--roster" aria-label="로스터">
      <h2>Roster (loading…)</h2>
    </section>
  );
}
