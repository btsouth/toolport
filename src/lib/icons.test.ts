import { describe, expect, it } from "vitest";
import { hasUnusableIcons, pickIconSrc, type McpIcon } from "./icons";

const png = "data:image/png;base64,iVBORw0KGgo=";

describe("pickIconSrc (SBS-708 / SEP-973)", () => {
  it("accepts a data: image and returns it unchanged", () => {
    expect(pickIconSrc([{ src: png }])).toBe(png);
    expect(pickIconSrc([{ src: `  ${png}  ` }])).toBe(png);
  });

  it("refuses remote URLs, which are a request rather than a picture", () => {
    // The security decision this file exists for: a remote icon URL means the app
    // contacts a host the server chose, every time the list paints — a beacon that
    // reveals when Toolport is open, with no tool call involved.
    for (const src of [
      "https://cdn.example.com/icon.png",
      "http://192.168.1.5/icon.png",
      "//cdn.example.com/icon.png",
      "/local/icon.png",
    ]) {
      expect(pickIconSrc([{ src }])).toBeNull();
    }
  });

  it("refuses non-image and script-capable data URIs", () => {
    for (const src of [
      "data:text/html;base64,PHNjcmlwdD4=",
      "data:application/javascript,alert(1)",
      "data:text/plain,hello",
      "javascript:alert(1)",
      "data:,",
    ]) {
      expect(pickIconSrc([{ src }])).toBeNull();
    }
  });

  it("allows SVG, which cannot execute script through an img element", () => {
    const svg = "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=";
    expect(pickIconSrc([{ src: svg }])).toBe(svg);
  });

  it("skips an oversized icon rather than inlining megabytes into every render", () => {
    const huge = `data:image/png;base64,${"A".repeat(70 * 1024)}`;
    expect(pickIconSrc([{ src: huge }])).toBeNull();
    // ...and still finds a usable one later in the list.
    expect(pickIconSrc([{ src: huge }, { src: png }])).toBe(png);
  });

  it("takes the first usable icon and ignores earlier refused ones", () => {
    const icons: McpIcon[] = [
      { src: "https://cdn.example.com/a.png" },
      { src: "data:text/html,x" },
      { src: png },
    ];
    expect(pickIconSrc(icons)).toBe(png);
  });

  it("handles missing, empty and malformed input without throwing", () => {
    expect(pickIconSrc(undefined)).toBeNull();
    expect(pickIconSrc(null)).toBeNull();
    expect(pickIconSrc([])).toBeNull();
    // A server can send anything; none of it should break a render.
    expect(pickIconSrc([{} as McpIcon])).toBeNull();
    expect(pickIconSrc([{ src: 123 } as unknown as McpIcon])).toBeNull();
    expect(pickIconSrc("nope" as unknown as McpIcon[])).toBeNull();
  });
});

describe("hasUnusableIcons", () => {
  it("distinguishes no icons from icons we refused", () => {
    expect(hasUnusableIcons(undefined)).toBe(false);
    expect(hasUnusableIcons([])).toBe(false);
    expect(hasUnusableIcons([{ src: png }])).toBe(false);
    expect(hasUnusableIcons([{ src: "https://cdn.example.com/a.png" }])).toBe(true);
  });
});
