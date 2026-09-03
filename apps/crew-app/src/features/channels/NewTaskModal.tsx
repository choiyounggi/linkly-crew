// New-task modal (plan D4, extended by t2-fe-picker D5/D7/D8/D10 for 2차 런
// 이슈 #13): task content + a 새로 만들기/기존 선택 project-source toggle +
// roster (RosterPanel reused) + a scripted toggle (default false — a real
// project is being created, so a real CLI run is the default; scripted demo
// is opt-in). Submit flow: create mode calls create_project(name) and
// forwards its returned path; existing mode skips create_project and
// forwards the selected project's path — either way startChannel(goal,
// scripted, projectRoot) never gets null (t2-fe-picker D10). Validation
// timing: fields stay enabled and the form validates on submit
// (wiki/frontend/forms/validation-timing.md) — blank required fields block
// the submit call and show inline errors, they never disable the button;
// the one exception is an in-flight project-list fetch, which blocks submit
// until it settles (wiki/frontend/data-fetching/async-ui-states.md).

import { useEffect, useRef, useState } from "react";

import { Button, Field } from "../../components/primitives";
import { defaultSource, useRunStore } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import type { ProjectInfo } from "../../lib/types";
import RosterPanel from "../roster";
import "./new-task-modal.css";

interface NewTaskModalProps {
  onClose: () => void;
  source?: RunEventSource;
}

type SubmitState = "idle" | "loading" | "error";
type Mode = "create" | "existing";
type ListState = "idle" | "loading" | "error" | "loaded";

/** t3-be-project's create_project error vocabulary (decisions.md), mapped to Korean. */
function projectErrorMessage(err: unknown): string {
  const raw = err instanceof Error ? err.message : String(err);
  if (raw === "invalid_name") {
    return "프로젝트명은 소문자·숫자·하이픈만 사용할 수 있습니다 (첫 글자는 소문자/숫자, 63자 이하)";
  }
  if (raw === "project_exists") return "이미 존재하는 프로젝트명입니다";
  if (raw === "gh_missing") return "GitHub CLI(gh)가 설치되어 있지 않습니다";
  if (raw === "gh_unauthenticated") return "GitHub CLI 로그인이 필요합니다 (gh auth login)";
  if (raw === "clone_failed") return "저장소 클론에 실패했습니다";
  if (raw.startsWith("create_failed")) {
    return `프로젝트 생성에 실패했습니다: ${raw.slice("create_failed:".length).trim()}`;
  }
  if (raw.startsWith("scaffold_failed")) {
    return `스캐폴딩에 실패했습니다: ${raw.slice("scaffold_failed:".length).trim()}`;
  }
  if (raw.startsWith("commit_failed")) {
    return `초기 커밋에 실패했습니다: ${raw.slice("commit_failed:".length).trim()}`;
  }
  return `프로젝트 생성 중 오류가 발생했습니다: ${raw}`;
}

/** `list_projects`의 `workspace_missing: <path>` reject를 빈 상태(R7)와 구분되는 문구로 매핑한다 (R6/D7). */
function projectListErrorMessage(err: unknown): string {
  const raw = err instanceof Error ? err.message : String(err);
  if (raw.startsWith("workspace_missing")) {
    return "워크스페이스 경로를 확인해 주세요 — 프로젝트 목록을 불러올 수 없습니다";
  }
  return `프로젝트 목록을 불러오지 못했습니다: ${raw}`;
}

const FOCUSABLE_SELECTOR =
  'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

export default function NewTaskModal({ onClose, source = defaultSource }: NewTaskModalProps) {
  const startChannel = useRunStore((s) => s.startChannel);

  const [goal, setGoal] = useState("");
  const [projectName, setProjectName] = useState("");
  const [scripted, setScripted] = useState(false);
  const [goalError, setGoalError] = useState<string | null>(null);
  const [projectNameError, setProjectNameError] = useState<string | null>(null);
  const [submitState, setSubmitState] = useState<SubmitState>("idle");
  const [submitError, setSubmitError] = useState<string | null>(null);

  // 새로 만들기/기존 선택 토글 (t2-fe-picker D5/D7/D8/D10). `listProjects`가 없는 소스에서는
  // supportsExistingMode가 false라 토글 자체가 렌더되지 않고, 이하 모드 관련 상태는 전부 미사용.
  const supportsExistingMode = typeof source.listProjects === "function";
  const [mode, setMode] = useState<Mode>("create");
  const [listState, setListState] = useState<ListState>("idle");
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [listError, setListError] = useState<string | null>(null);
  const [selected, setSelected] = useState<ProjectInfo | null>(null);
  const [selectedError, setSelectedError] = useState<string | null>(null);

  const dialogRef = useRef<HTMLDivElement | null>(null);
  const firstFieldRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    firstFieldRef.current?.focus();
  }, []);

  // Esc closes; Tab/Shift+Tab wraps focus inside the dialog (focus trap).
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key !== "Tab") return;
      const root = dialogRef.current;
      if (!root) return;
      const focusable = Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR));
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  // "기존 선택" 모드로 전환될 때(모달이 열릴 때가 아니라)와 재시도 버튼에서 호출한다 (D7).
  const fetchProjects = async () => {
    if (!source.listProjects) return;
    setListState("loading");
    setListError(null);
    try {
      const result = await source.listProjects();
      setProjects(result);
      setListState("loaded");
    } catch (err) {
      setListError(projectListErrorMessage(err));
      setListState("error");
    }
  };

  const handleModeChange = (next: Mode) => {
    setMode(next);
    setProjectNameError(null);
    setSelectedError(null);
    if (next === "existing" && listState === "idle") {
      void fetchProjects();
    }
  };

  const handleSubmit = async () => {
    const trimmedGoal = goal.trim();
    const nextGoalError = trimmedGoal ? null : "작업 내용을 입력하세요";
    setGoalError(nextGoalError);

    if (mode === "create") {
      const trimmedName = projectName.trim();
      const nextProjectNameError = trimmedName ? null : "프로젝트명을 입력하세요";
      setProjectNameError(nextProjectNameError);
      if (nextGoalError || nextProjectNameError) return;

      setSubmitState("loading");
      setSubmitError(null);
      try {
        const info = await source.createProject(trimmedName);
        await startChannel(trimmedGoal, scripted, info.path);
        setSubmitState("idle");
        onClose();
      } catch (err) {
        setSubmitError(projectErrorMessage(err));
        setSubmitState("error");
      }
      return;
    }

    // existing 모드: 목록 조회가 아직 진행 중이면 그것이 끝날 때까지 제출을 막는다 (D8).
    if (listState === "loading") return;
    const nextSelectedError = selected ? null : "기존 프로젝트를 선택하세요";
    setSelectedError(nextSelectedError);
    if (nextGoalError || nextSelectedError) return;

    setSubmitState("loading");
    setSubmitError(null);
    try {
      await startChannel(trimmedGoal, scripted, selected!.path);
      setSubmitState("idle");
      onClose();
    } catch (err) {
      setSubmitError(err instanceof Error ? err.message : String(err));
      setSubmitState("error");
    }
  };

  return (
    <div
      className="modal-overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="modal new-task-modal" role="dialog" aria-modal="true" aria-label="새 작업" ref={dialogRef}>
        <h2 className="new-task-modal__title">새 작업</h2>

        <Field label="작업 내용" error={goalError}>
          <textarea
            ref={firstFieldRef}
            rows={4}
            value={goal}
            onChange={(e) => setGoal(e.target.value)}
            placeholder="무엇을 만들까요?"
          />
        </Field>

        {supportsExistingMode && (
          <div className="new-task-modal__mode-toggle" role="group" aria-label="프로젝트 선택 방식">
            <button
              type="button"
              className={`new-task-modal__mode-btn${mode === "create" ? " is-active" : ""}`}
              aria-pressed={mode === "create"}
              onClick={() => handleModeChange("create")}
            >
              새로 만들기
            </button>
            <button
              type="button"
              className={`new-task-modal__mode-btn${mode === "existing" ? " is-active" : ""}`}
              aria-pressed={mode === "existing"}
              onClick={() => handleModeChange("existing")}
            >
              기존 선택
            </button>
          </div>
        )}

        {mode === "create" ? (
          <Field label="프로젝트명" error={projectNameError}>
            <input
              type="text"
              value={projectName}
              onChange={(e) => setProjectName(e.target.value)}
              placeholder="my-project"
            />
          </Field>
        ) : (
          <div className="new-task-modal__existing">
            {listState === "loading" && <p className="new-task-modal__list-status">불러오는 중…</p>}

            {listState === "error" && (
              <div className="new-task-modal__list-status new-task-modal__list-status--error" role="alert">
                <p>{listError}</p>
                <Button variant="ghost" size="sm" onClick={() => void fetchProjects()}>
                  다시 시도
                </Button>
              </div>
            )}

            {listState === "loaded" && projects.length === 0 && (
              <p className="new-task-modal__list-status">
                기존 프로젝트가 없습니다. "새로 만들기"를 사용해 주세요.
              </p>
            )}

            {listState === "loaded" && projects.length > 0 && (
              <fieldset className="new-task-modal__project-list">
                <legend>기존 프로젝트</legend>
                {projects.map((p) => (
                  <label key={p.path} className="new-task-modal__project-item">
                    <input
                      type="radio"
                      name="existing-project"
                      checked={selected?.path === p.path}
                      onChange={() => {
                        setSelected(p);
                        setSelectedError(null);
                      }}
                    />
                    {p.name}
                  </label>
                ))}
              </fieldset>
            )}

            {selectedError && (
              <p className="new-task-modal__error" role="alert">
                {selectedError}
              </p>
            )}
          </div>
        )}

        <label className="new-task-modal__scripted">
          <input type="checkbox" checked={scripted} onChange={(e) => setScripted(e.target.checked)} />
          시나리오 데모로 실행 (scripted)
        </label>

        <RosterPanel source={source} />

        {submitState === "error" && submitError && (
          <p className="new-task-modal__error" role="alert">
            {submitError}
          </p>
        )}

        <div className="new-task-modal__actions">
          <Button variant="ghost" disabled={submitState === "loading"} onClick={onClose}>
            취소
          </Button>
          <Button
            variant="primary"
            loading={submitState === "loading"}
            disabled={mode === "existing" && listState === "loading"}
            onClick={() => void handleSubmit()}
          >
            시작
          </Button>
        </div>
      </div>
    </div>
  );
}
