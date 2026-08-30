import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ClientLogo } from "./ClientLogo";

describe("ClientLogo", () => {
  it.each([
    ["anythingllm", "AnythingLLM", "AN"],
    ["boltai", "BoltAI", "BO"],
    ["continue", "Continue", "CO"],
    ["droid", "Factory Droid", "FD"],
    ["omp", "Oh My Pi", "OM"],
  ])("uses the neutral fallback for %s", (id, name, initials) => {
    render(<ClientLogo id={id} name={name} />);

    expect(screen.getByText(initials)).toBeInTheDocument();
  });

  it("still renders a verified vendored mark", () => {
    const { container } = render(<ClientLogo id="codex" name="Codex" />);

    expect(container.querySelector("svg title")?.textContent).toBe("Codex");
  });
});
