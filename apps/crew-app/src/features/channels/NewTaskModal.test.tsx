import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useRunStore } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import { MockEventSource } from "../../lib/mock-source";
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

    expect(await screen.findByText("GitHub CLI(gh)가 설치되어 있지 않습니다")).toHaveAttribute("role", "alert");
    expect(startChannel).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "새 작업" })).toBeInTheDocument();
    startChannel.mockRestore();
  });
});

describe("NewTaskModal — issue #15: startChannel failure after createProject success", () => {
  it("#15 run-start failure after createProject success shows a distinct Korean message, not 프로젝트 생성 (R2)", async () => {
    const createProject = vi.fn(async () => ({ name: "my-app", path: "/ws/my-app" }));
    const source = fakeSource({ createProject });
    const startChannel = vi
      .spyOn(useRunStore.getState(), "startChannel")
      .mockRejectedValueOnce(new Error("project_root invalid: not a git repo"));
    const onClose = vi.fn();

    render(<NewTaskModal onClose={onClose} source={source} />);

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.change(screen.getByLabelText("프로젝트명"), { target: { value: "my-app" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    const alert = await screen.findByText(/는 생성되었지만 실행 시작에 실패했습니다/);
    expect(alert).toHaveAttribute("role", "alert");
    expect(alert.textContent).toBe(
      '프로젝트 "my-app"(/ws/my-app)는 생성되었지만 실행 시작에 실패했습니다: project_root invalid: not a git repo',
    );
    expect(createProject).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();

    startChannel.mockRestore();
  });

  it("#15 retry after a run-start failure calls startChannel only, not createProject again (R3)", async () => {
    const createProject = vi.fn(async () => ({ name: "my-app", path: "/ws/my-app" }));
    const source = fakeSource({ createProject });
    const startChannel = vi
      .spyOn(useRunStore.getState(), "startChannel")
      .mockRejectedValueOnce(new Error("project_root invalid: not a git repo"))
      .mockResolvedValueOnce("run-1");
    const onClose = vi.fn();

    render(<NewTaskModal onClose={onClose} source={source} />);

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.change(screen.getByLabelText("프로젝트명"), { target: { value: "my-app" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));
    await screen.findByText(/실행 시작에 실패했습니다/);

    fireEvent.click(screen.getByRole("button", { name: "시작" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));

    expect(createProject).toHaveBeenCalledTimes(1);
    expect(startChannel).toHaveBeenCalledTimes(2);
    expect(startChannel).toHaveBeenNthCalledWith(2, "goal", false, "/ws/my-app");

    startChannel.mockRestore();
  });

  it("#15 switching mode after a run-start failure resets the retry, back to create re-calls createProject (R4)", async () => {
    const createProject = vi.fn(async () => ({ name: "my-app", path: "/ws/my-app" }));
    const source = fakeSource({ createProject, listProjects: vi.fn(async () => []) });
    const startChannel = vi
      .spyOn(useRunStore.getState(), "startChannel")
      .mockRejectedValueOnce(new Error("project_root invalid: not a git repo"))
      .mockResolvedValueOnce("run-1");

    render(<NewTaskModal onClose={() => {}} source={source} />);

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.change(screen.getByLabelText("프로젝트명"), { target: { value: "my-app" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));
    await screen.findByText(/실행 시작에 실패했습니다/);

    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));
    fireEvent.click(screen.getByRole("button", { name: "새로 만들기" }));

    fireEvent.change(screen.getByLabelText("프로젝트명"), { target: { value: "my-app" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    await waitFor(() => expect(createProject).toHaveBeenCalledTimes(2));

    startChannel.mockRestore();
  });

  it("#15 existing-mode raw message: startChannel failure in existing mode still shows the raw error text (R5)", async () => {
    const createProject = vi.fn();
    const listProjects = vi.fn(async () => [{ name: "demo", path: "/ws/demo" }]);
    const source = fakeSource({ createProject, listProjects });
    const startChannel = vi
      .spyOn(useRunStore.getState(), "startChannel")
      .mockRejectedValueOnce(new Error("boom"));

    render(<NewTaskModal onClose={() => {}} source={source} />);
    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));
    await screen.findByText("demo");
    fireEvent.click(screen.getByLabelText("demo"));
    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("boom");
    expect(createProject).not.toHaveBeenCalled();

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
    expect(document.querySelectorAll(".new-task-modal__project-item")).toHaveLength(1);
    expect(document.querySelector(".new-task-modal__actions")).not.toBeNull();
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
    expect(document.querySelector(".new-task-modal__actions")).not.toBeNull();

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
    expect(document.querySelector(".new-task-modal__actions")).not.toBeNull();
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

  it("boundary: 20 existing projects still render one project-list and keep actions/roster mounted", async () => {
    const listProjects = vi.fn(async () =>
      Array.from({ length: 20 }, (_, i) => ({ name: `p${i}`, path: `/ws/p${i}` })),
    );
    const source = fakeSource({ listProjects });

    render(<NewTaskModal onClose={() => {}} source={source} />);
    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));

    await screen.findByText("p0");

    expect(document.querySelectorAll(".new-task-modal__project-list")).toHaveLength(1);
    expect(document.querySelectorAll(".new-task-modal__project-item")).toHaveLength(20);
    expect(document.querySelector(".new-task-modal__actions")).not.toBeNull();
    expect(document.querySelector(".new-task-modal .panel--roster")).not.toBeNull();
  });
});

describe("NewTaskModal — issue #33: auto-detected DoD check preview", () => {
  const CREATE_MODE_NOTE =
    "새 프로젝트에는 시작 시 Cargo.toml/package.json이 없어 DoD 체크가 감지되지 않습니다 — 첫 런은 Completed/푸시에 도달할 수 없습니다. 기존 레포를 선택하면 체크가 자동 감지됩니다.";
  const NO_CHECKS_WARNING = /감지된 DoD 체크가 없습니다 — Cargo.toml 또는 test\/build 스크립트가 있는 package.json이 없으면 이 런은 Completed\/푸시에 도달할 수 없습니다/;

  async function selectExisting(projects: { name: string; path: string }[], overrides: Partial<RunEventSource>) {
    const source = fakeSource({ listProjects: vi.fn(async () => projects), ...overrides });
    render(<NewTaskModal onClose={() => {}} source={source} />);
    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));
    await screen.findByText(projects[0].name);
    fireEvent.click(screen.getByLabelText(projects[0].name));
    return source;
  }

  it("normal: lists each detected check for the selected root and allows starting", async () => {
    const mock = new MockEventSource();
    const previewRunChecks = vi.fn((root: string | null) => mock.previewRunChecks(root));
    const startChannel = vi.spyOn(useRunStore.getState(), "startChannel").mockResolvedValue("run-1");
    await selectExisting([{ name: "web", path: "/ws/web" }], { previewRunChecks });

    const list = await screen.findByRole("list", { name: "Developer 태스크의 DoD 체크:" });
    expect(Array.from(list.querySelectorAll("li"), (li) => li.textContent)).toEqual([
      "npm test (기대: exit 0)",
      "npm run build (기대: exit 0)",
    ]);
    expect(previewRunChecks).toHaveBeenCalledWith("/ws/web");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));
    await waitFor(() => expect(startChannel).toHaveBeenCalledWith("goal", false, "/ws/web"));
    startChannel.mockRestore();
  });

  it("boundary: a root with no detected checks shows the warning, and starting is still allowed", async () => {
    const mock = new MockEventSource();
    const startChannel = vi.spyOn(useRunStore.getState(), "startChannel").mockResolvedValue("run-1");
    await selectExisting([{ name: "bare", path: "/ws/no-checks" }], {
      previewRunChecks: (root) => mock.previewRunChecks(root),
    });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(NO_CHECKS_WARNING);
    expect(screen.queryByText("Developer 태스크의 DoD 체크:")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));
    await waitFor(() => expect(startChannel).toHaveBeenCalledWith("goal", false, "/ws/no-checks"));
    startChannel.mockRestore();
  });

  it("create mode (the default) always shows the no-checks note, and starting is still allowed", async () => {
    const startChannel = vi.spyOn(useRunStore.getState(), "startChannel").mockResolvedValue("run-1");
    render(<NewTaskModal onClose={() => {}} source={fakeSource()} />);

    expect(screen.getByRole("alert")).toHaveTextContent(CREATE_MODE_NOTE);

    fireEvent.change(screen.getByLabelText("작업 내용"), { target: { value: "goal" } });
    fireEvent.change(screen.getByLabelText("프로젝트명"), { target: { value: "my-app" } });
    fireEvent.click(screen.getByRole("button", { name: "시작" }));
    await waitFor(() => expect(startChannel).toHaveBeenCalledWith("goal", false, "/tmp/my-app"));
    startChannel.mockRestore();
  });

  it("boundary: existing mode without a selected root previews nothing and renders neither list nor alert", async () => {
    const previewRunChecks = vi.fn(async () => [{ run: "cargo test", expect: "exit 0" }]);
    const source = fakeSource({ listProjects: vi.fn(async () => [{ name: "demo", path: "/ws/demo" }]), previewRunChecks });
    render(<NewTaskModal onClose={() => {}} source={source} />);

    fireEvent.click(screen.getByRole("button", { name: "기존 선택" }));
    await screen.findByText("demo");

    expect(previewRunChecks).not.toHaveBeenCalled();
    expect(screen.queryByText(/DoD 체크/)).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("error: a rejected preview renders the warning plus the error line instead of throwing", async () => {
    const previewRunChecks = vi.fn(async () => {
      throw new Error("preview exploded");
    });
    await selectExisting([{ name: "demo", path: "/ws/demo" }], { previewRunChecks });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(NO_CHECKS_WARNING);
    expect(alert).toHaveTextContent("DoD 체크 감지 실패: preview exploded");
    expect(document.querySelector(".new-task-modal__actions")).not.toBeNull();
  });

  it("switching back to create mode clears the preview", async () => {
    const mock = new MockEventSource();
    await selectExisting([{ name: "web", path: "/ws/web" }], { previewRunChecks: (root) => mock.previewRunChecks(root) });
    await screen.findByText("npm test (기대: exit 0)");

    fireEvent.click(screen.getByRole("button", { name: "새로 만들기" }));

    await waitFor(() => expect(screen.queryByText("Developer 태스크의 DoD 체크:")).not.toBeInTheDocument());
    expect(screen.queryByText("npm test (기대: exit 0)")).not.toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(CREATE_MODE_NOTE);
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
