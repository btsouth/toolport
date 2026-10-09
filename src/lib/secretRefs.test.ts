import { describe, it, expect } from "vitest";
import { secretReferenceReview } from "./secretRefs";
import type { ServerEntry } from "./types";
describe("reference review", () => {
  it("shows the provider, exact reference, destination and mapped header", () => {
    const s = {id:"r",name:"Refs",transport:"http",command:null,args:[],url:"https://service.example/mcp",source:"team:t",env:[{key:"TOKEN",secret:true,value:null,source:{ref:"op://Private/GitHub Token/credential"}}],headerKeys:[{key:"X-Api-Key",env:"TOKEN"}]} satisfies ServerEntry;
    expect(secretReferenceReview(s)).toContain("1Password entry op://Private/GitHub Token/credential will be sent to https://service.example/mcp (header:X-Api-Key)");
  });
});
