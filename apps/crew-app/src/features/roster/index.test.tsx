import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import RosterPanel from "./index";

describe("RosterPanel — stub", () => {
  it("mounts a labeled placeholder panel (t-ui-roster fills in the real content)", () => {
    render(<RosterPanel />);
    expect(screen.getByLabelText("로스터")).toBeInTheDocument();
    expect(screen.getByText("Roster (loading…)")).toBeInTheDocument();
  });
});
