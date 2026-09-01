// contract: t1-foundation owns the implementation.
//
// 소비자(t2-chrome / t3-messaging / t4-worksurfaces)는 이 시그니처에 대고 계획한다.
// 시그니처를 바꿔야 한다면 plan gap 으로 보고한다 — 소비자 3개가 여기 묶여 있다.

import { cloneElement, isValidElement, useId, type ReactElement, type ReactNode } from "react";

import "./primitives.css";

export type ButtonProps = {
  variant: "primary" | "danger" | "ghost";
  size?: "sm" | "md";
  loading?: boolean;
  disabled?: boolean;
  onClick?: () => void;
  children: ReactNode;
};

export type IconButtonProps = {
  label: string;
  disabled?: boolean;
  onClick?: () => void;
  children: ReactNode;
};

export type BadgeProps = {
  variant: "neutral" | "accent" | "danger" | "success";
  children: ReactNode;
};

export type PanelProps = {
  title?: ReactNode;
  level?: 1 | 2;
  children: ReactNode;
};

export type FieldProps = {
  label: ReactNode;
  error?: ReactNode;
  children: ReactNode;
};

export function Button({ variant, size = "md", loading = false, disabled = false, onClick, children }: ButtonProps) {
  return (
    <button
      type="button"
      className={`btn btn--${variant} btn--${size}${loading ? " is-loading" : ""}`}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

export function IconButton({ label, disabled = false, onClick, children }: IconButtonProps) {
  return (
    <button type="button" className="icon-btn" aria-label={label} disabled={disabled} onClick={onClick}>
      {children}
    </button>
  );
}

export function Badge({ variant, children }: BadgeProps) {
  return (
    <span className={`badge badge--${variant}`} data-testid="badge">
      {children}
    </span>
  );
}

export function Panel({ title, level = 1, children }: PanelProps) {
  return (
    <section className={`panel panel--level-${level}`}>
      {title !== undefined && <h2 className="panel__title">{title}</h2>}
      <div className="panel__body">{children}</div>
    </section>
  );
}

export function Field({ label, error, children }: FieldProps) {
  const inputId = useId();
  const errorId = useId();
  const input = isValidElement(children)
    ? cloneElement(children as ReactElement<{ id?: string; "aria-describedby"?: string; "aria-invalid"?: boolean }>, {
        id: inputId,
        "aria-describedby": error ? errorId : undefined,
        "aria-invalid": error ? true : undefined,
      })
    : children;

  return (
    <div className={`field${error ? " field--error" : ""}`}>
      <label className="field__label" htmlFor={inputId}>
        {label}
      </label>
      {input}
      {error && (
        <p className="field__error" id={errorId} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
