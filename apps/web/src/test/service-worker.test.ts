import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("PWA offline shell", () => {
  it("precaches the built application shell without runtime business-data caching", () => {
    const source = readFileSync(resolve(process.cwd(), "src/service-worker.ts"), "utf8");
    expect(source).toContain("precacheAndRoute(self.__WB_MANIFEST)");
    expect(source).not.toContain("NetworkFirst");
    expect(source).not.toContain("/api/");
  });
});
