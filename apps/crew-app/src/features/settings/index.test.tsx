import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { TerminalPanelProps } from "../../components/terminal";
import SettingsMenu from "./index";

vi.mock("../../components/terminal", () => ({
  default: (props: TerminalPanelProps) => <div data-testid="fake-terminal" data-inject-text={props.injectText ?? ""} />,
}));

afterEach(() => {
  vi.clearAllMocks();
});

describe("SettingsMenu", () => {
  // -- normal: reuses OnboardingPanels (D1 — same inner panel, no dup impl) --

  it("renders the settings landmark reusing the same OnboardingPanels workspace field as onboarding", async () => {
    render(<SettingsMenu onClose={vi.fn()} />);

    expect(screen.getByLabelText("설정")).toBeInTheDocument();
    const input = await screen.findByLabelText("워크스페이스 경로");
    await waitFor(() => expect(input).toHaveValue("~/linkly-crew/workspace"));
  });

  // -- boundary: re-entry has no first-run completion gating ----------------

  it("does not render the 시작하기/건너뛰기 completion footer (re-entry, not first-run)", async () => {
    render(<SettingsMenu onClose={vi.fn()} />);

    await screen.findByLabelText("워크스페이스 경로");
    expect(screen.queryByRole("button", { name: "시작하기" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "건너뛰기" })).not.toBeInTheDocument();
  });

  // -- error/interaction: close button calls onClose, not onComplete --------

  it("calls onClose exactly once when the close button is clicked", async () => {
    const onClose = vi.fn();
    render(<SettingsMenu onClose={onClose} />);

    await screen.findByLabelText("워크스페이스 경로");
    fireEvent.click(screen.getByRole("button", { name: "닫기" }));

    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
