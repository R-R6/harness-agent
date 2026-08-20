import { describe, expect, it } from "vitest";
import { buildXtermOptions, detectWindowsPty } from "../xtermRuntime";

describe("buildXtermOptions", () => {
  it("does not convert LF for PTY-backed TUI hosts", () => {
    const options = buildXtermOptions({ userAgent: "Mozilla/5.0", platform: "Linux x86_64" });
    expect(options.convertEol).toBe(false);
    expect(options.cursorBlink).toBe(false);
    expect(options.lineHeight).toBe(1);
    expect(options.windowsPty).toBeUndefined();
  });

  it("enables ConPTY viewport compensation on Windows without faking a build number", () => {
    const options = buildXtermOptions({
      userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
      platform: "Win32",
    });
    expect(options.windowsPty).toEqual({ backend: "conpty" });
    expect(options.windowsPty).not.toHaveProperty("buildNumber");
  });
});

describe("detectWindowsPty", () => {
  it("ignores non-Windows user agents", () => {
    expect(detectWindowsPty({ userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" })).toBeUndefined();
  });

  it("treats navigator.platform Win32 as Windows even without a Windows UA token", () => {
    expect(detectWindowsPty({ userAgent: "Mozilla/5.0", platform: "Win32" })).toEqual({ backend: "conpty" });
  });
});
