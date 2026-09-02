import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { TerminalPanelProps } from "../../components/terminal";
import OnboardingFlow from "./index";

vi.mock("../../components/terminal", () => ({
  default: (props: TerminalPanelProps) => <div data-testid="fake-terminal" data-inject-text={props.injectText ?? ""} />,
}));

afterEach(() => {
  vi.clearAllMocks();
});

describe("OnboardingFlow", () => {
  // -- normal: mounts the shared OnboardingPanels wizard inside the onboarding landmark --

  it("renders the onboarding section, wrapping OnboardingPanels with the workspace field prefilled", async () => {
    render(<OnboardingFlow onComplete={vi.fn()} />);

    expect(screen.getByLabelText("온보딩")).toBeInTheDocument();
    const input = await screen.findByLabelText("워크스페이스 경로");
    // OnboardingFlow doesn't accept an `api` prop (locked stub signature —
    // only `onComplete`), so it always uses the default adapter; under this
    // jsdom test env isTauri() is false, so it resolves its fixed mock data.
    await waitFor(() => expect(input).toHaveValue("~/linkly-crew/workspace"));
  });

  // -- boundary: mounting alone must never call onComplete ------------------

  it("does not call onComplete on mount — only a user action (시작하기/건너뛰기) may trigger it", async () => {
    const onComplete = vi.fn();
    render(<OnboardingFlow onComplete={onComplete} />);

    await screen.findByLabelText("워크스페이스 경로");
    expect(onComplete).not.toHaveBeenCalled();
  });

  // -- normal (end-to-end through the real default api's mock data): 시작하기 -> onComplete --

  it("calls onComplete when 시작하기 is clicked once the default mock data satisfies the required tools", async () => {
    const onComplete = vi.fn();
    render(<OnboardingFlow onComplete={onComplete} />);

    const startButton = await screen.findByRole("button", { name: "시작하기" });
    await waitFor(() => expect(startButton).toBeEnabled());

    fireEvent.click(startButton);
    expect(onComplete).toHaveBeenCalledTimes(1);
  });
});
