import { describe, expect, it, vi } from "vitest";
import {
  buildXtermOptions,
  createOutputStabilizer,
  detectWindowsPty,
  pinCursorSteady,
  CODEX_CURSOR_PIN,
  steadyDecscusrParam,
} from "../xtermRuntime";

describe("buildXtermOptions", () => {
  it("does not convert LF for PTY-backed TUI hosts", () => {
    const options = buildXtermOptions({ userAgent: "Mozilla/5.0", platform: "Linux x86_64" });
    expect(options.convertEol).toBe(false);
    expect(options.cursorBlink).toBe(false);
    expect(options.lineHeight).toBe(1);
    expect(options.scrollback).toBe(0);
    expect(options.rescaleOverlappingGlyphs).toBe(false);
    expect(options.fontFamily).toContain("NSimSun");
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

describe("createOutputStabilizer", () => {
  it("strips blink-on / cursor-show and keeps the hardware cursor hidden", () => {
    const stabilizer = createOutputStabilizer();
    expect(stabilizer.push("\x1b[?12h")).toBe(CODEX_CURSOR_PIN);
    expect(stabilizer.push("\x1b[?25h")).toBe(CODEX_CURSOR_PIN);
    expect(stabilizer.push("\x1b[1 q")).toBe(CODEX_CURSOR_PIN);
    expect(stabilizer.push("A\x1b[?25hB")).toBe(`AB${CODEX_CURSOR_PIN}`);
  });

  it("holds a CSI split across chunks", () => {
    const stabilizer = createOutputStabilizer();
    expect(stabilizer.push("pre\x1b[?2")).toBe(`pre${CODEX_CURSOR_PIN}`);
    expect(stabilizer.push("5hpost")).toBe(`post${CODEX_CURSOR_PIN}`);
  });
});

describe("pinCursorSteady", () => {
  it("maps blinking DECSCUSR styles to the matching steady cursor", () => {
    expect(steadyDecscusrParam(1)).toBe(2);
    expect(steadyDecscusrParam(5)).toBe(6);
  });

  it("swallows DECSCUSR and cursor blink/hide modes", () => {
    const handlers = new Map<string, (params: Array<number | number[]>) => boolean>();
    const terminal = {
      options: { cursorBlink: true, cursorStyle: "block" as const },
      parser: {
        registerCsiHandler: vi.fn((id: { prefix?: string; intermediates?: string; final: string }, handler: (params: Array<number | number[]>) => boolean) => {
          handlers.set(`${id.prefix ?? ""}${id.intermediates ?? ""}${id.final}`, handler);
          return { dispose: vi.fn() };
        }),
      },
      onWriteParsed: vi.fn(() => ({ dispose: vi.fn() })),
    };

    pinCursorSteady(terminal as never);
    expect(handlers.get(" q")?.([1])).toBe(true);
    expect(terminal.options.cursorBlink).toBe(false);
    expect(handlers.get("?h")?.([25])).toBe(true);
  });
});
