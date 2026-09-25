import { describe, expect, it } from "vitest";
import { serverLogoKey } from "@/lib/serverLogo";

describe("serverLogoKey", () => {
  it("maps curated providers and full API variants to their marks", () => {
    expect(serverLogoKey("Stripe (Full API)")).toBe("stripe");
    expect(serverLogoKey("Cloudflare Docs")).toBe("cloudflare");
    expect(serverLogoKey("Linear")).toBe("linear");
    expect(serverLogoKey("Atlassian")).toBe("atlassian");
    expect(serverLogoKey("Postman")).toBe("postman");
    expect(serverLogoKey("Redis")).toBe("redis");
    expect(serverLogoKey("Jira Production")).toBe("jira");
  });

  it("leaves unknown servers on the neutral transport fallback", () => {
    expect(serverLogoKey("My private MCP")).toBeNull();
  });
});
