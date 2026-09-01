// contract: t1-foundation owns the implementation.
//
// 이 파일은 계약 스텁이다 — 시그니처만 확정하고 구현은 t1-foundation 이 채운다.
// 소비자(t2-chrome / t3-messaging / t4-worksurfaces)는 이 시그니처에 대고
// 계획한다. t1 이 머지되기 전에 호출하면 의도적으로 throw 한다.

import type { ReactNode } from "react";

const NOT_IMPLEMENTED = "primitives: t1-foundation 미머지 — 계약 스텁";

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

export function Button(_props: ButtonProps): never {
  throw new Error(NOT_IMPLEMENTED);
}

export function IconButton(_props: IconButtonProps): never {
  throw new Error(NOT_IMPLEMENTED);
}

export function Badge(_props: BadgeProps): never {
  throw new Error(NOT_IMPLEMENTED);
}

export function Panel(_props: PanelProps): never {
  throw new Error(NOT_IMPLEMENTED);
}

export function Field(_props: FieldProps): never {
  throw new Error(NOT_IMPLEMENTED);
}
