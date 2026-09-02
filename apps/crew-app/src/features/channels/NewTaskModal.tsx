// New-task modal (plan D4): task content + project name + roster (RosterPanel
// reused) + a scripted toggle (default false — a real project is being
// created, so a real CLI run is the default; scripted demo is opt-in).
// Submit flow: create_project(name) -> only on success, startChannel(goal,
// scripted) (store's start_run). Validation timing: fields stay enabled and
// the form validates on submit (wiki/frontend/forms/validation-timing.md) —
// blank required fields block the submit call and show inline errors, they
// never disable the button.

import { useEffect, useRef, useState } from "react";

import { Button, Field } from "../../components/primitives";
import { defaultSource, useRunStore } from "../../lib/store";
import type { RunEventSource } from "../../lib/source";
import RosterPanel from "../roster";
import "./new-task-modal.css";

interface NewTaskModalProps {
  onClose: () => void;
  source?: RunEventSource;
}

type SubmitState = "idle" | "loading" | "error";

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

  const handleSubmit = async () => {
    const trimmedGoal = goal.trim();
    const trimmedName = projectName.trim();
    const nextGoalError = trimmedGoal ? null : "작업 내용을 입력하세요";
    const nextProjectNameError = trimmedName ? null : "프로젝트명을 입력하세요";
    setGoalError(nextGoalError);
    setProjectNameError(nextProjectNameError);
    if (nextGoalError || nextProjectNameError) return;

    setSubmitState("loading");
    setSubmitError(null);
    try {
      await source.createProject(trimmedName);
      await startChannel(trimmedGoal, scripted);
      setSubmitState("idle");
      onClose();
    } catch (err) {
      setSubmitError(projectErrorMessage(err));
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

        <Field label="프로젝트명" error={projectNameError}>
          <input
            type="text"
            value={projectName}
            onChange={(e) => setProjectName(e.target.value)}
            placeholder="my-project"
          />
        </Field>

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
          <Button variant="primary" loading={submitState === "loading"} onClick={() => void handleSubmit()}>
            시작
          </Button>
        </div>
      </div>
    </div>
  );
}
