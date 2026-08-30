// Roster panel (contracts-m5.md §C7b): preset dropdown, per-slot harness +
// model editing, save, and a per-slot swap button. `source` is injectable
// (plan D2) so tests don't need a global mock; defaults to the app's
// `defaultSource` (t-ui-core, src/lib/store.ts).

import { useEffect, useState } from "react";

import { defaultSource, useRunStore } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { HarnessInfo, Roster, RosterAgent, RosterPreset } from "../../lib/types";
import "./roster.css";

interface RosterPanelProps {
  source?: RunEventSource;
}

const ADDABLE_ROLES = ["pm", "designer", "publisher", "developer", "qa"] as const;

type LoadState = "loading" | "idle" | "error";
type SaveState = "idle" | "loading" | "success" | "error";

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export default function RosterPanel({ source = defaultSource }: RosterPanelProps) {
  const canSwap = useRunStore((s) => s.runId !== null && s.finished === null);

  const supportsPresets = typeof source.listPresets === "function";
  const supportsHarnesses = typeof source.detectHarnesses === "function";
  const supportsGetRoster = typeof source.getRoster === "function";
  const supportsSetRoster = typeof source.setRoster === "function";
  const supportsSwap = typeof source.swapHarness === "function";
  const supported = supportsPresets || supportsHarnesses || supportsGetRoster || supportsSetRoster || supportsSwap;

  const [draft, setDraft] = useState<Roster | null>(null);
  const [presets, setPresets] = useState<RosterPreset[]>([]);
  const [harnesses, setHarnesses] = useState<HarnessInfo[]>([]);
  const [selectedPresetName, setSelectedPresetName] = useState("");
  const [addRole, setAddRole] = useState("");

  const [loadState, setLoadState] = useState<LoadState>(supported ? "loading" : "idle");
  const [loadError, setLoadError] = useState<string | null>(null);

  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [saveError, setSaveError] = useState<string | null>(null);

  const [swapLoadingId, setSwapLoadingId] = useState<string | null>(null);
  const [swapErrors, setSwapErrors] = useState<Record<string, string>>({});

  useEffect(() => {
    if (!supported) return;
    let cancelled = false;
    setLoadState("loading");
    setLoadError(null);

    Promise.all([
      supportsPresets ? source.listPresets!() : Promise.resolve<RosterPreset[]>([]),
      supportsHarnesses ? source.detectHarnesses!() : Promise.resolve<HarnessInfo[]>([]),
      supportsGetRoster ? source.getRoster!() : Promise.resolve<Roster | null>(null),
    ])
      .then(([loadedPresets, loadedHarnesses, loadedRoster]) => {
        if (cancelled) return;
        setPresets(loadedPresets);
        setHarnesses(loadedHarnesses);
        setDraft(loadedRoster);
        setLoadState("idle");
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setLoadError(errorMessage(err));
        setLoadState("error");
      });

    return () => {
      cancelled = true;
    };
  }, [source, supported, supportsPresets, supportsHarnesses, supportsGetRoster]);

  if (!supported) {
    return (
      <section className="panel panel--roster" aria-label="로스터">
        <h2>로스터</h2>
        <p className="roster__status">이 소스에서 지원 안 함</p>
      </section>
    );
  }

  const handlePresetChange = (name: string) => {
    setSelectedPresetName(name);
    const preset = presets.find((p) => p.name === name);
    if (preset) {
      setDraft({ agents: preset.roster.agents.map((a) => ({ ...a })) });
    }
  };

  const updateSlot = (agentId: string, patch: Partial<Pick<RosterAgent, "harness" | "model">>) => {
    setDraft((prev) => (prev ? { agents: prev.agents.map((a) => (a.id === agentId ? { ...a, ...patch } : a)) } : prev));
  };

  const missingRoles = draft ? ADDABLE_ROLES.filter((role) => !draft.agents.some((a) => a.role === role)) : [];
  const effectiveAddRole = missingRoles.includes(addRole as (typeof ADDABLE_ROLES)[number])
    ? addRole
    : (missingRoles[0] ?? "");

  const handleAddSlot = () => {
    if (!draft || !effectiveAddRole) return;
    const role = effectiveAddRole;
    const newAgent: RosterAgent = {
      id: `agent:${role}`,
      role,
      harness: "claude-code",
      model: "default",
      instructions: "",
    };
    setDraft({ agents: [...draft.agents, newAgent] });
  };

  const handleDeleteSlot = (agentId: string) => {
    setDraft((prev) => (prev ? { agents: prev.agents.filter((a) => a.id !== agentId) } : prev));
  };

  const handleSave = async () => {
    if (!draft || !supportsSetRoster) return;
    setSaveState("loading");
    setSaveError(null);
    try {
      await source.setRoster!(draft);
      setSaveState("success");
    } catch (err) {
      setSaveError(errorMessage(err));
      setSaveState("error");
    }
  };

  const handleSwap = async (agentId: string, harness: string) => {
    if (!supportsSwap) return;
    setSwapLoadingId(agentId);
    setSwapErrors((prev) => {
      const { [agentId]: _removed, ...rest } = prev;
      return rest;
    });
    try {
      await source.swapHarness!(agentId, harness);
    } catch (err) {
      setSwapErrors((prev) => ({ ...prev, [agentId]: errorMessage(err) }));
    } finally {
      setSwapLoadingId((current) => (current === agentId ? null : current));
    }
  };

  return (
    <section className="panel panel--roster" aria-label="로스터">
      <h2>로스터</h2>

      {loadState === "loading" && <p className="roster__status">불러오는 중…</p>}
      {loadState === "error" && (
        <p className="roster__status roster__status--error">불러오기 실패: {loadError}</p>
      )}

      {supportsPresets && (
        <div className="roster__presets">
          <label htmlFor="roster-preset-select">프리셋</label>
          <select
            id="roster-preset-select"
            value={selectedPresetName}
            onChange={(e) => handlePresetChange(e.target.value)}
          >
            <option value="">선택…</option>
            {presets.map((p) => (
              <option key={p.name} value={p.name}>
                {p.name}
              </option>
            ))}
          </select>
        </div>
      )}

      {supportsGetRoster ? (
        <ul className="roster__slots">
          {draft && draft.agents.length > 0 ? (
            draft.agents.map((agent) => {
              const swapping = swapLoadingId === agent.id;
              const swapError = swapErrors[agent.id];
              return (
                <li key={agent.id} className="roster__slot">
                  <span className="roster__slot-role">{agent.role}</span>
                  {supportsHarnesses ? (
                    <select
                      aria-label={`${agent.role} 하네스`}
                      value={agent.harness}
                      onChange={(e) => updateSlot(agent.id, { harness: e.target.value })}
                    >
                      {harnesses.map((h) => (
                        <option key={h.id} value={h.id} disabled={!h.installed || h.adapter === "none"}>
                          {h.id}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <span className="roster__slot-harness">{agent.harness}</span>
                  )}
                  <input
                    aria-label={`${agent.role} 모델`}
                    type="text"
                    value={agent.model}
                    onChange={(e) => updateSlot(agent.id, { model: e.target.value })}
                  />
                  {supportsSwap && (
                    <button
                      type="button"
                      aria-label={`${agent.role} 교체`}
                      disabled={!canSwap || swapping}
                      onClick={() => handleSwap(agent.id, agent.harness)}
                    >
                      {swapping ? "교체 중…" : "교체"}
                    </button>
                  )}
                  <button
                    type="button"
                    aria-label={`슬롯 삭제 ${agent.id}`}
                    disabled={agent.role === "lead"}
                    onClick={() => handleDeleteSlot(agent.id)}
                  >
                    삭제
                  </button>
                  {swapError && <span className="roster__slot-error">{swapError}</span>}
                </li>
              );
            })
          ) : (
            <li className="roster__slot roster__slot--empty">슬롯 없음</li>
          )}
        </ul>
      ) : (
        <p className="roster__status">슬롯 편집: 이 소스에서 지원 안 함</p>
      )}

      {supportsGetRoster && draft && (
        <div className="roster__add">
          <label htmlFor="roster-add-role-select">역할 추가</label>
          <select
            id="roster-add-role-select"
            aria-label="추가할 역할"
            value={effectiveAddRole}
            disabled={missingRoles.length === 0}
            onChange={(e) => setAddRole(e.target.value)}
          >
            {missingRoles.length === 0 ? (
              <option value="">전부 배정됨</option>
            ) : (
              missingRoles.map((role) => (
                <option key={role} value={role}>
                  {role}
                </option>
              ))
            )}
          </select>
          <button type="button" onClick={handleAddSlot} disabled={missingRoles.length === 0}>
            추가
          </button>
        </div>
      )}

      {supportsSetRoster && (
        <div className="roster__save">
          <button type="button" onClick={() => void handleSave()} disabled={!draft || saveState === "loading"}>
            {saveState === "loading" ? "저장 중…" : "저장"}
          </button>
          {saveState === "success" && <span className="roster__save-status">저장됨</span>}
          {saveState === "error" && (
            <span className="roster__save-status roster__save-status--error">저장 실패: {saveError}</span>
          )}
        </div>
      )}
    </section>
  );
}
