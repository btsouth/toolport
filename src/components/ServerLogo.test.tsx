import { describe, expect, it } from "vitest";
import { serverLogoKey } from "@/lib/serverLogo";

describe("serverLogoKey", () => {
  it("maps curated providers and full API variants to their marks", () => {
    expect(serverLogoKey("Stripe (Full API)")).toBe("stripe");
    expect(serverLogoKey("Cloudflare Docs")).toBe("cloudflare");
    expect(serverLogoKey("Linear")).toBe("linear");
    expect(serverLogoKey("Trello (work)")).toBe("trello");
    expect(serverLogoKey("RevenueCat")).toBe("revenuecat");
    expect(serverLogoKey("Revenue Cat (personal)")).toBe("revenuecat");
    expect(serverLogoKey("Atlassian")).toBe("atlassian");
    expect(serverLogoKey("Postman")).toBe("postman");
    expect(serverLogoKey("Redis")).toBe("redis");
    expect(serverLogoKey("Jira Production")).toBe("jira");
  });

  it("gives the reference servers an icon only as the leading word", () => {
    expect(serverLogoKey("Linode")).toBe("linode");
    expect(serverLogoKey("Exa Search")).toBe("exa");
    expect(serverLogoKey("Hexagon")).toBeNull();
    expect(serverLogoKey("Time")).toBe("time");
    expect(serverLogoKey("Sequential Thinking")).toBe("sequentialthinking");
    expect(serverLogoKey("Showtime")).toBeNull();
    expect(serverLogoKey("Team memory")).toBeNull();
  });

  it("leaves unknown servers on the neutral transport fallback", () => {
    expect(serverLogoKey("My private MCP")).toBeNull();
  });
});
