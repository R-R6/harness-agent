import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const capability = JSON.parse(
  readFileSync(join(process.cwd(), "src-tauri", "capabilities", "default.json"), "utf8"),
);

describe("native window theme capability", () => {
  it("allows the frontend to synchronize the native title bar theme", () => {
    expect(capability.permissions).toContain("core:window:allow-set-theme");
  });
});
