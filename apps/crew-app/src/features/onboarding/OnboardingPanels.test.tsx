import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { TerminalPanelProps } from "../../components/terminal";
import type { OnboardingApi } from "../../lib/onboarding-api";
import type { ToolStatus } from "../../lib/types";
import OnboardingPanels from "./OnboardingPanels";

// OnboardingPanels' own responsibility is *constructing* injectText and
// clearing it via onInjected — not xterm/pty wiring (t5-terminal owns that,
// components/terminal 수정 금지). A lightweight fake stands in for the real
// TerminalPanel so these tests assert exactly the props OnboardingPanels
// passes it, without touching xterm/jsdom internals.
const terminalPropsLog: TerminalPanelProps[] = [];
vi.mock("../../components/terminal", () => ({
  default: (props: TerminalPanelProps) => {
    terminalPropsLog.push(props);
    return <div data-testid="fake-terminal" data-inject-text={props.injectText ?? ""} />;
  },
}));

afterEach(() => {
  terminalPropsLog.length = 0;
  vi.clearAllMocks();
});

const GIT_OK: ToolStatus = { id: "git", installed: true, path: "/usr/bin/git", version: "git version 2.43.0", install_command: "brew install git" };
const CLAUDE_OK: ToolStatus = {
  id: "claude",
  installed: true,
  path: "/usr/local/bin/claude",
  version: "1.0.0",
  install_command: "npm install -g @anthropic-ai/claude-code",
};
const GH_AUTHENTICATED: ToolStatus = {
  id: "gh",
  installed: true,
  path: "/usr/local/bin/gh",
  version: "gh version 2.60.0",
  install_command: "brew install gh",
  authenticated: true,
};
const GH_UNAUTHENTICATED: ToolStatus = { ...GH_AUTHENTICATED, authenticated: false };
const CODEX_MISSING: ToolStatus = { id: "codex", installed: false, path: null, version: null, install_command: "npm install -g @openai/codex" };

function createFakeApi(overrides: Partial<OnboardingApi> = {}): OnboardingApi {
  return {
    getSettings: vi.fn(async () => ({ workspace_root: "/home/dev/workspace" })),
    setSettings: vi.fn(async (workspaceRoot: string) => ({ workspace_root: workspaceRoot })),
    onboardingStatus: vi.fn(async () => [GIT_OK, GH_AUTHENTICATED, CLAUDE_OK, CODEX_MISSING]),
    ...overrides,
  };
}

describe("OnboardingPanels", () => {
  // -- normal: workspace prefill + save --------------------------------

  it("prefills the workspace field from getSettings and saves an edited value via setSettings", async () => {
    const setSettings = vi.fn(async (workspaceRoot: string) => ({ workspace_root: workspaceRoot }));
    const api = createFakeApi({ setSettings });

    render(<OnboardingPanels api={api} variant="onboarding" onComplete={vi.fn()} />);

    const input = await screen.findByLabelText("워크스페이스 경로");
    await waitFor(() => expect(input).toHaveValue("/home/dev/workspace"));

    fireEvent.change(input, { target: { value: "/home/dev/new-workspace" } });
    fireEvent.click(screen.getByRole("button", { name: "저장" }));

    await waitFor(() => expect(setSettings).toHaveBeenCalledWith("/home/dev/new-workspace"));
    expect(await screen.findByText("저장됨")).toBeInTheDocument();
  });

  // -- error: workspace save surfaces the rejection ----------------------

  it("surfaces a setSettings rejection (relative-path Err) under the workspace field", async () => {
    const api = createFakeApi({
      setSettings: vi.fn(async () => {
        throw new Error('workspace_root must be an absolute path, got "relative/workspace"');
      }),
    });

    render(<OnboardingPanels api={api} variant="onboarding" onComplete={vi.fn()} />);

    const input = await screen.findByLabelText("워크스페이스 경로");
    fireEvent.change(input, { target: { value: "relative/workspace" } });
    fireEvent.click(screen.getByRole("button", { name: "저장" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/absolute/);
  });

  // -- boundary/security: inject sends the exact command, no trailing Enter --

  it("passes the exact install command as injectText on click, and clears it once TerminalPanel reports onInjected", async () => {
    const api = createFakeApi({ onboardingStatus: vi.fn(async () => [GIT_OK, GH_AUTHENTICATED, CLAUDE_OK, CODEX_MISSING]) });

    render(<OnboardingPanels api={api} variant="onboarding" onComplete={vi.fn()} />);

    await screen.findByText("codex");
    fireEvent.click(screen.getByRole("button", { name: "터미널에 붙여넣기" }));

    await waitFor(() => {
      const last = terminalPropsLog[terminalPropsLog.length - 1];
      expect(last.injectText).toBe("npm install -g @openai/codex");
    });
    // D5/t5 security boundary: no trailing newline — Enter is the user's own keypress.
    expect(terminalPropsLog[terminalPropsLog.length - 1].injectText).not.toMatch(/\n$/);

    const lastProps = terminalPropsLog[terminalPropsLog.length - 1];
    act(() => {
      lastProps.onInjected?.();
    });

    await waitFor(() => {
      const last = terminalPropsLog[terminalPropsLog.length - 1];
      expect(last.injectText).toBeNull();
    });
  });

  it("offers the same copy/inject UX for gh auth login when gh is installed but unauthenticated", async () => {
    const api = createFakeApi({ onboardingStatus: vi.fn(async () => [GIT_OK, GH_UNAUTHENTICATED, CLAUDE_OK]) });

    render(<OnboardingPanels api={api} variant="onboarding" onComplete={vi.fn()} />);

    await screen.findByText("미인증");
    const injectButtons = screen.getAllByRole("button", { name: "터미널에 붙여넣기" });
    expect(injectButtons).toHaveLength(1);
    fireEvent.click(injectButtons[0]);

    await waitFor(() => {
      const last = terminalPropsLog[terminalPropsLog.length - 1];
      expect(last.injectText).toBe("gh auth login");
    });
  });

  // -- completion condition (D5) ------------------------------------------

  it("enables 시작하기 once git/gh(authenticated)/claude are all satisfied, and calls onComplete", async () => {
    const onComplete = vi.fn();
    const api = createFakeApi({ onboardingStatus: vi.fn(async () => [GIT_OK, GH_AUTHENTICATED, CLAUDE_OK]) });

    render(<OnboardingPanels api={api} variant="onboarding" onComplete={onComplete} />);

    const startButton = await screen.findByRole("button", { name: "시작하기" });
    await waitFor(() => expect(startButton).toBeEnabled());
    expect(screen.queryByText(/필수 도구/)).not.toBeInTheDocument();

    fireEvent.click(startButton);
    expect(onComplete).toHaveBeenCalledTimes(1);
  });

  it("disables 시작하기 and shows a warning when required tools are missing, but 건너뛰기 still calls onComplete", async () => {
    const onComplete = vi.fn();
    const api = createFakeApi({ onboardingStatus: vi.fn(async () => [GIT_OK, GH_UNAUTHENTICATED, CODEX_MISSING]) });

    render(<OnboardingPanels api={api} variant="onboarding" onComplete={onComplete} />);

    const startButton = await screen.findByRole("button", { name: "시작하기" });
    await waitFor(() => expect(screen.getByText(/필수 도구/)).toBeInTheDocument());
    expect(startButton).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "건너뛰기" }));
    expect(onComplete).toHaveBeenCalledTimes(1);
  });

  // -- recheck race guard (D6) ----------------------------------------------

  it("applies only the latest recheck response when 다시 확인 is clicked before the first call resolves", async () => {
    const resolvers: Array<(v: ToolStatus[]) => void> = [];
    const onboardingStatus = vi.fn(() => new Promise<ToolStatus[]>((resolve) => resolvers.push(resolve)));
    const api = createFakeApi({ onboardingStatus });

    render(<OnboardingPanels api={api} variant="onboarding" onComplete={vi.fn()} />);

    // Mount effect fires call #1.
    await waitFor(() => expect(onboardingStatus).toHaveBeenCalledTimes(1));
    act(() => resolvers[0]([GIT_OK, GH_UNAUTHENTICATED, CODEX_MISSING]));
    await screen.findByText("codex");

    // Two rapid rechecks in a row -> calls #2 and #3.
    fireEvent.click(screen.getByRole("button", { name: "다시 확인" }));
    fireEvent.click(screen.getByRole("button", { name: "확인 중…" }));
    await waitFor(() => expect(onboardingStatus).toHaveBeenCalledTimes(3));

    // Resolve out of order: call #3 (latest) lands first, with requirements satisfied.
    act(() => resolvers[2]([GIT_OK, GH_AUTHENTICATED, CLAUDE_OK]));
    await waitFor(() => expect(screen.getByRole("button", { name: "시작하기" })).toBeEnabled());

    // Stale call #2 resolves afterwards with requirements unsatisfied — must be ignored.
    act(() => resolvers[1]([GIT_OK, GH_UNAUTHENTICATED, CODEX_MISSING]));
    expect(screen.getByRole("button", { name: "시작하기" })).toBeEnabled();
  });

  // -- variant "settings": no completion footer ----------------------------

  it("hides the 시작하기/건너뛰기 footer in settings variant", async () => {
    const api = createFakeApi({ onboardingStatus: vi.fn(async () => [GIT_OK, GH_AUTHENTICATED, CLAUDE_OK]) });

    render(<OnboardingPanels api={api} variant="settings" />);

    await screen.findByLabelText("워크스페이스 경로");
    expect(screen.queryByRole("button", { name: "시작하기" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "건너뛰기" })).not.toBeInTheDocument();
  });
});
