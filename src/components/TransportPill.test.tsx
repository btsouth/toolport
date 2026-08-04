import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { TransportPill } from "./TransportPill";

describe("TransportPill", () => {
  it("exposes its visible transport label to assistive technology", () => {
    render(<TransportPill transport="stdio" />);

    expect(screen.getByRole("img", { name: "Transport: stdio" })).toBeInTheDocument();
  });
});
