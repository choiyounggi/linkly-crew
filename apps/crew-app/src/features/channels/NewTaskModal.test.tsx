import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useRunStore } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import NewTaskModal from "./NewTaskModal";

function fakeSource(overrides: Partial<RunEventSource> = {}): RunEventSource {
  return {
    start: vi.fn(async () => "run-1"),
    onEvent: vi.fn(() => () => {}),
    stop: vi.fn(async () => {}),
    remove: vi.fn(async () => {}),
    resync: vi.fn(async () => {}),
    listRuns: vi.fn(async () => []),
    createProject: vi.fn(async (name: string) => ({ name, path: `/tmp/${name}` })),
    listPresets: vi.fn(async () => []),
    detectHarnesses: vi.fn(async () => []),
    getRoster: vi.fn(async () => ({ agents: [] })),
    setRoster: vi.fn(async () => {}),
    ...overrides,
  };
}

afterEach(() => {
  useRunStore.setState({ activeRunId: null, channelOrder: [], channels: {} });
});

describe("NewTaskModal — normal: success flow (R2/R3)", () => {
  it("calls createProject via the injected source, then the store's startChannel, and closes on success", async () => {
    // NewTaskModal's `source` prop drives createProject/RosterPanel directly
    // (like RosterPanel's own `source` prop); the actual run-starting flow
    // is the global store's startChannel action (same as Sidebar's
    // selectChannel/closeChannel) — spy on that action rather than on a
    // swapped-out low-level source, which store.test.ts already covers.
    const createProject = vi.fn(async (name: string) => ({ name, path: `/ws/${name}` }));
    const source = fakeSource({ createProject });
    const startChannel = vi.spyOn(useRunStore.getState(), "startChannel").mockResolvedValue("run-1");
    const onClose = vi.fn();

    render(<NewTaskModal onClose={onClose} source={source} />);

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "랜딩 페이지 만들기" } });
    fireEvent.change(screen.getByLabelText("프로젝트명"), { target: { value: "my-app" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    await waitFor(() => expect(createProject).toHaveBeenCalledWith("my-app"));
    await waitFor(() => expect(startChannel).toHaveBeenCalledWith("랜딩 페이지 만들기", false, "/ws/my-app"));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));

    startChannel.mockRestore();
  });

  it("defaults the scripted toggle to false (D4) and passes it through to startChannel", async () => {
    const createProject = vi.fn(async (name: string) => ({ name, path: `/ws/${name}` }));
    const startChannel = vi.spyOn(useRunStore.getState(), "startChannel").mockResolvedValue("run-1");
    render(<NewTaskModal onClose={() => {}} source={fakeSource({ createProject })} />);

    expect(screen.getByLabelText(/시나리오 데모로 실행/)).not.toBeChecked();

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.change(screen.getByLabelText("프로젝트명"), { target: { value: "my-app" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    await waitFor(() => expect(startChannel).toHaveBeenCalledWith("goal", false, "/ws/my-app"));
    startChannel.mockRestore();
  });
});

describe("NewTaskModal — error: gh_missing surfaces a Korean message and keeps the modal open (R3)", () => {
  it("shows the mapped error and does not call startChannel/onClose", async () => {
    const startChannel = vi.spyOn(useRunStore.getState(), "startChannel").mockResolvedValue("run-1");
    const source = fakeSource({
      createProject: vi.fn(async () => {
        throw new Error("gh_missing");
      }),
    });
    const onClose = vi.fn();

    render(<NewTaskModal onClose={onClose} source={source} />);

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.change(screen.getByLabelText("프로젝트명"), { target: { value: "my-app" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("GitHub CLI(gh)가 설치되어 있지 않습니다");
    expect(startChannel).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "새 작업" })).toBeInTheDocument();
    startChannel.mockRestore();
  });
});

describe("NewTaskModal — boundary: blank required fields block submission (D4/validation-timing)", () => {
  it("shows inline field errors and never calls createProject for blank goal/projectName", () => {
    const createProject = vi.fn();
    const source = fakeSource({ createProject });

    render(<NewTaskModal onClose={() => {}} source={source} />);
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    expect(screen.getByText("작업 내용을 입력하세요")).toBeInTheDocument();
    expect(screen.getByText("프로젝트명을 입력하세요")).toBeInTheDocument();
    expect(createProject).not.toHaveBeenCalled();
    // Submit stays enabled (wiki: never disable for invalid state, only while in flight).
    expect(screen.getByRole("button", { name: "시작" })).toBeEnabled();
  });
});

describe("NewTaskModal — 기존 선택 모드 (t2-fe-picker R5/R6/R7/R8/D5)", () => {
  it("R5: submits the selected project's path and never calls createProject", async () => {
    const createProject = vi.fn();
    const listProjects = vi.fn(async () => [{ name: "demo", path: "/ws/demo" }]);
    const source = fakeSource({ createProject, listProjects });
    const startChannel = vi.spyOn(useRunStore.getState(), "startChannel").mockResolvedValue("run-1");

    render(<NewTaskModal onClose={() => {}} source={source} />);
    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));

    await screen.findByText("demo");
    fireEvent.click(screen.getByLabelText("demo"));
    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    await waitFor(() => expect(startChannel).toHaveBeenCalledWith("goal", false, "/ws/demo"));
    expect(createProject).not.toHaveBeenCalled();

    startChannel.mockRestore();
  });

  it("R6: workspace_missing shows a workspace-specific message, not the empty-state message, with a working retry", async () => {
    const listProjects = vi
      .fn()
      .mockRejectedValueOnce(new Error("workspace_missing: /ws"))
      .mockResolvedValueOnce([{ name: "demo", path: "/ws/demo" }]);
    const source = fakeSource({ listProjects });

    render(<NewTaskModal onClose={() => {}} source={source} />);
    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("워크스페이스 경로를 확인해 주세요");
    expect(screen.queryByText(/기존 프로젝트가 없습니다/)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "다시 시도" }));

    await screen.findByText("demo");
    expect(listProjects).toHaveBeenCalledTimes(2);
  });

  it("R7: an empty project list shows the deliberate empty-state message, distinct from the error message", async () => {
    const listProjects = vi.fn(async () => []);
    const source = fakeSource({ listProjects });

    render(<NewTaskModal onClose={() => {}} source={source} />);
    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));

    expect(await screen.findByText(/기존 프로젝트가 없습니다/)).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("R8: submitting with nothing selected shows an inline error and never calls startChannel", async () => {
    const listProjects = vi.fn(async () => [{ name: "demo", path: "/ws/demo" }]);
    const source = fakeSource({ listProjects });
    const startChannel = vi.spyOn(useRunStore.getState(), "startChannel").mockResolvedValue("run-1");

    render(<NewTaskModal onClose={() => {}} source={source} />);
    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));
    await screen.findByText("demo");

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    expect(await screen.findByText("기존 프로젝트를 선택하세요")).toBeInTheDocument();
    expect(startChannel).not.toHaveBeenCalled();

    startChannel.mockRestore();
  });

  it("D5 boundary: a source without listProjects renders no 기존 선택 toggle, and create mode behaves unchanged", () => {
    const source = fakeSource();
    expect(source.listProjects).toBeUndefined();

    render(<NewTaskModal onClose={() => {}} source={source} />);

    expect(screen.queryByRole("button", { name: "기존 선택" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "새로 만들기" })).not.toBeInTheDocument();
    expect(screen.getByLabelText("프로젝트명")).toBeInTheDocument();
  });
});

describe("NewTaskModal — accessibility: dialog role, Esc closes", () => {
  it("exposes role=dialog/aria-modal and closes on Escape", () => {
    const onClose = vi.fn();
    render(<NewTaskModal onClose={onClose} source={fakeSource()} />);

    const dialog = screen.getByRole("dialog", { name: "새 작업" });
    expect(dialog).toHaveAttribute("aria-modal", "true");

    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("closes when the overlay backdrop is clicked, but not when the dialog content is clicked", () => {
    const onClose = vi.fn();
    const { container } = render(<NewTaskModal onClose={onClose} source={fakeSource()} />);

    fireEvent.mouseDown(screen.getByRole("dialog", { name: "새 작업" }));
    expect(onClose).not.toHaveBeenCalled();

    const overlay = container.querySelector(".modal-overlay")!;
    fireEvent.mouseDown(overlay);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
