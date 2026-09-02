// Shared inner panel (plan D1): both OnboardingFlow (first-run wizard) and
// SettingsMenu (re-entry) render this same component — one implementation,
// no duplicated workspace/checklist/terminal logic. `variant` only toggles
// the completion footer (시작하기/건너뛰기), which only makes sense during
// first-run onboarding.

import { useEffect, useRef, useState } from "react";

import { defaultOnboardingApi, type OnboardingApi } from "../../lib/onboarding-api";
import type { ToolStatus } from "../../lib/types";
import { Badge, Button, Field } from "../../components/primitives";
import TerminalPanel from "../../components/terminal";
import "./onboarding.css";

export interface OnboardingPanelsProps {
  api?: OnboardingApi;
  variant: "onboarding" | "settings";
  /** Called by 시작하기/건너뛰기 (variant "onboarding" only, plan D5 — both call onComplete). */
  onComplete?: () => void;
}

type LoadState = "loading" | "idle" | "error";
type SaveState = "idle" | "loading" | "success" | "error";

/** Required for 시작하기 to enable (plan D5): git + gh(installed AND authenticated) + claude. */
const REQUIRED_IDS = ["git", "gh", "claude"];

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function isToolSatisfied(status: ToolStatus): boolean {
  if (!status.installed) return false;
  if (status.id === "gh") return status.authenticated === true;
  return true;
}

function requiredSatisfied(statuses: ToolStatus[]): boolean {
  return REQUIRED_IDS.every((id) => {
    const status = statuses.find((s) => s.id === id);
    return status ? isToolSatisfied(status) : false;
  });
}

async function copyToClipboard(text: string): Promise<void> {
  try {
    await navigator.clipboard?.writeText(text);
  } catch {
    // Clipboard access can be denied by the browser/OS; the command is
    // still visible in the code block, so this is a silent no-op.
  }
}

export default function OnboardingPanels({ api = defaultOnboardingApi, variant, onComplete }: OnboardingPanelsProps) {
  // -- workspace step (R1) -------------------------------------------------
  const [workspaceRoot, setWorkspaceRoot] = useState("");
  const [workspaceLoadState, setWorkspaceLoadState] = useState<LoadState>("loading");
  const [workspaceSaveState, setWorkspaceSaveState] = useState<SaveState>("idle");
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setWorkspaceLoadState("loading");
    api
      .getSettings()
      .then((settings) => {
        if (cancelled) return;
        setWorkspaceRoot(settings.workspace_root);
        setWorkspaceLoadState("idle");
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setWorkspaceError(errorMessage(err));
        setWorkspaceLoadState("error");
      });
    return () => {
      cancelled = true;
    };
  }, [api]);

  const handleSaveWorkspace = async () => {
    setWorkspaceSaveState("loading");
    setWorkspaceError(null);
    try {
      const saved = await api.setSettings(workspaceRoot);
      setWorkspaceRoot(saved.workspace_root);
      setWorkspaceSaveState("success");
    } catch (err) {
      setWorkspaceError(errorMessage(err));
      setWorkspaceSaveState("error");
    }
  };

  // -- tool checklist (R2/R4/R6) -------------------------------------------
  const [statuses, setStatuses] = useState<ToolStatus[]>([]);
  const [statusLoadState, setStatusLoadState] = useState<LoadState>("loading");
  const [statusError, setStatusError] = useState<string | null>(null);
  // Race guard (plan D6): only the latest recheck's response is applied.
  const requestSeqRef = useRef(0);

  const fetchStatuses = () => {
    const seq = ++requestSeqRef.current;
    setStatusLoadState("loading");
    setStatusError(null);
    api
      .onboardingStatus()
      .then((result) => {
        if (requestSeqRef.current !== seq) return;
        setStatuses(result);
        setStatusLoadState("idle");
      })
      .catch((err: unknown) => {
        if (requestSeqRef.current !== seq) return;
        setStatusError(errorMessage(err));
        setStatusLoadState("error");
      });
  };

  useEffect(fetchStatuses, [api]);

  // -- embedded terminal (R3) ----------------------------------------------
  const [injectText, setInjectText] = useState<string | null>(null);
  const injectCommand = (command: string) => setInjectText(command);

  // -- completion (R5) ------------------------------------------------------
  const requiredOk = requiredSatisfied(statuses);

  return (
    <div className="onboarding-panels">
      <section className="onboarding-panels__section">
        <h2 className="onboarding-panels__section-title">워크스페이스</h2>
        <Field label="워크스페이스 경로" error={workspaceError ?? undefined}>
          <input
            type="text"
            value={workspaceRoot}
            disabled={workspaceLoadState === "loading"}
            onChange={(e) => setWorkspaceRoot(e.target.value)}
          />
        </Field>
        <div className="onboarding-panels__footer">
          <Button
            variant="primary"
            loading={workspaceSaveState === "loading"}
            disabled={workspaceLoadState === "loading"}
            onClick={() => void handleSaveWorkspace()}
          >
            {workspaceSaveState === "loading" ? "저장 중…" : "저장"}
          </Button>
          {workspaceSaveState === "success" && <span className="onboarding-panels__status-ok">저장됨</span>}
        </div>
      </section>

      <section className="onboarding-panels__section">
        <div className="onboarding-panels__section-header">
          <h2 className="onboarding-panels__section-title">도구 체크리스트</h2>
          {/* Not `loading`-disabled (plan D6): a real double-click must be able to
              fire two overlapping onboardingStatus() calls so the race guard
              (requestSeqRef) has something to guard against. */}
          <Button variant="ghost" size="sm" onClick={fetchStatuses}>
            {statusLoadState === "loading" ? "확인 중…" : "다시 확인"}
          </Button>
        </div>

        {statusLoadState === "error" && (
          <p className="onboarding-panels__error" role="alert">
            확인 실패: {statusError}
          </p>
        )}

        <ul className="onboarding-tool-list">
          {statuses.map((status) => {
            const required = REQUIRED_IDS.includes(status.id);
            const satisfied = isToolSatisfied(status);
            return (
              <li key={status.id} className="onboarding-tool">
                <div className="onboarding-tool__row">
                  <span className="onboarding-tool__id">{status.id}</span>
                  <Badge variant={satisfied ? "success" : required ? "danger" : "neutral"}>
                    {status.installed ? "설치됨" : "미설치"}
                  </Badge>
                  {status.id === "gh" && status.installed && (
                    <Badge variant={status.authenticated ? "success" : "danger"}>
                      {status.authenticated ? "인증됨" : "미인증"}
                    </Badge>
                  )}
                  {status.version && <span className="onboarding-tool__version">{status.version}</span>}
                </div>

                {!status.installed && (
                  <div className="onboarding-tool__install">
                    <pre className="onboarding-tool__command">{status.install_command}</pre>
                    <div className="onboarding-tool__install-actions">
                      <Button variant="ghost" size="sm" onClick={() => void copyToClipboard(status.install_command)}>
                        복사
                      </Button>
                      <Button variant="ghost" size="sm" onClick={() => injectCommand(status.install_command)}>
                        터미널에 붙여넣기
                      </Button>
                    </div>
                    <p className="onboarding-tool__install-note">
                      터미널 입력줄에 채워지기만 합니다 — 실행하려면 Enter를 직접 눌러주세요.
                    </p>
                  </div>
                )}

                {status.id === "gh" && status.installed && !status.authenticated && (
                  <div className="onboarding-tool__install">
                    <pre className="onboarding-tool__command">gh auth login</pre>
                    <div className="onboarding-tool__install-actions">
                      <Button variant="ghost" size="sm" onClick={() => void copyToClipboard("gh auth login")}>
                        복사
                      </Button>
                      <Button variant="ghost" size="sm" onClick={() => injectCommand("gh auth login")}>
                        터미널에 붙여넣기
                      </Button>
                    </div>
                    <p className="onboarding-tool__install-note">
                      터미널 입력줄에 채워지기만 합니다 — 실행하려면 Enter를 직접 눌러주세요.
                    </p>
                  </div>
                )}
              </li>
            );
          })}
        </ul>

        <div className="onboarding-panels__terminal">
          <TerminalPanel injectText={injectText} onInjected={() => setInjectText(null)} />
        </div>
      </section>

      {variant === "onboarding" && (
        <div className="onboarding-panels__footer">
          {!requiredOk && (
            <p className="onboarding-panels__warning">
              필수 도구(git·gh 인증·claude)가 아직 준비되지 않았습니다. 건너뛰면 나중에 설정에서 다시 확인할 수 있습니다.
            </p>
          )}
          <Button variant="ghost" onClick={() => onComplete?.()}>
            건너뛰기
          </Button>
          <Button variant="primary" disabled={!requiredOk} onClick={() => onComplete?.()}>
            시작하기
          </Button>
        </div>
      )}
    </div>
  );
}
