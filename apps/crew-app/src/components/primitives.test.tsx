import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { Badge, Button, Field, IconButton, Panel } from "./primitives";

describe("Button", () => {
  it("renders with variant=primary and calls onClick when clicked", () => {
    const onClick = vi.fn();
    render(
      <Button variant="primary" onClick={onClick}>
        시작
      </Button>,
    );
    const button = screen.getByRole("button", { name: "시작" });
    fireEvent.click(button);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("does not call onClick when disabled (boundary)", () => {
    const onClick = vi.fn();
    render(
      <Button variant="primary" disabled onClick={onClick}>
        시작
      </Button>,
    );
    fireEvent.click(screen.getByRole("button", { name: "시작" }));
    expect(onClick).not.toHaveBeenCalled();
  });
});

describe("IconButton", () => {
  it("exposes label as the accessible name", () => {
    render(
      <IconButton label="설정 열기">
        <svg aria-hidden="true" />
      </IconButton>,
    );
    expect(screen.getByRole("button", { name: "설정 열기" })).toBeInTheDocument();
  });
});

describe("Field", () => {
  it("renders and associates the error message with the input (error case)", () => {
    render(
      <Field label="이메일" error="이메일 형식이 올바르지 않습니다">
        <input type="email" />
      </Field>,
    );
    const input = screen.getByRole("textbox", { name: "이메일" });
    const errorMessage = screen.getByText("이메일 형식이 올바르지 않습니다");
    expect(input).toHaveAccessibleDescription("이메일 형식이 올바르지 않습니다");
    expect(errorMessage).toBeInTheDocument();
  });
});

describe("Panel", () => {
  it("renders children with no title (boundary)", () => {
    render(
      <Panel>
        <p>본문 내용</p>
      </Panel>,
    );
    expect(screen.getByText("본문 내용")).toBeInTheDocument();
  });
});

describe("Badge", () => {
  it("does not crash with empty string children (boundary)", () => {
    render(<Badge variant="neutral">{""}</Badge>);
    expect(screen.getByTestId("badge")).toBeInTheDocument();
    expect(screen.getByTestId("badge")).toHaveTextContent("");
  });
});
