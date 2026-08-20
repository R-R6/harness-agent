import type { FitAddon } from "@xterm/addon-fit";
import type { ITerminalOptions, ITheme, IWindowsPty, Terminal } from "@xterm/xterm";

export interface XtermRuntime {
  Terminal: typeof Terminal;
  FitAddon: typeof FitAddon;
}

export const TERMINAL_THEME: ITheme = {
  background: "#0b0e13",
  foreground: "#d7dee8",
  cursor: "#8fb5ff",
  selectionBackground: "#27456f",
  black: "#11161d",
  red: "#ef8f8f",
  green: "#8bd5a5",
  yellow: "#e7c77d",
  blue: "#8fb5ff",
  magenta: "#c1a2f2",
  cyan: "#83d5d5",
  white: "#f2f4f7",
  brightBlack: "#566171",
};

interface NavigatorLike {
  userAgent?: string;
  platform?: string;
}

/**
 * ConPTY does not restore scrollback into the viewport when rows grow.
 * xterm needs `windowsPty.backend = "conpty"` or maximize/fit will leave a
 * ghost copy of the previous Ink/ratatui frame (Claude Code / Codex TUIs).
 *
 * Do not invent a Windows build number. xterm only enables ConPTY reflow when
 * `buildNumber >= 21376`; a fake 22621 would turn reflow on for older Win10
 * hosts and mis-wrap TUI lines.
 */
export function detectWindowsPty(
  env: NavigatorLike = typeof navigator === "undefined" ? {} : navigator,
): IWindowsPty | undefined {
  const ua = env.userAgent ?? "";
  const platform = env.platform ?? "";
  if (!/Windows/i.test(ua) && platform !== "Win32") return undefined;
  return { backend: "conpty" };
}

/**
 * Options for hosting Claude Code (Ink) and Codex (fullscreen TUI) over a PTY.
 *
 * `convertEol` must stay false: xterm docs say PTY/termios already translates
 * `\n` → `\r\n`. Enabling it makes TUI redraws stack as a second UI.
 */
export function buildXtermOptions(env?: NavigatorLike): ITerminalOptions {
  const windowsPty = detectWindowsPty(env);
  return {
    convertEol: false,
    cursorBlink: false,
    cursorInactiveStyle: "outline",
    // Cascadia/Consolas have no CJK glyphs. Without a monospace CJK fallback,
    // Windows substitutes Microsoft YaHei (proportional), so 你好 is not two
    // cells wide and Ink/Claude's cursor jumps a whole row.
    fontFamily: "Cascadia Mono, Consolas, NSimSun, SimSun, monospace",
    fontSize: 13,
    lineHeight: 1,
    // ConPTY + windowsPty pushes the old viewport into scrollback when rows
    // grow (maximize/fit). Ink/ratatui then look like two stacked UIs.
    scrollback: 0,
    // Overlapping CJK fallback glyphs get squashed into one cell and Ink's
    // cursor then appears a row away from 你好.
    rescaleOverlappingGlyphs: false,
    theme: TERMINAL_THEME,
    ...(windowsPty ? { windowsPty } : {}),
  };
}

/** Load the canvas-backed renderer only after the terminal workspace opens. */
export async function loadXtermRuntime(): Promise<XtermRuntime> {
  const [{ Terminal }, { FitAddon }] = await Promise.all([
    import("@xterm/xterm"),
    import("@xterm/addon-fit"),
  ]);
  return { Terminal, FitAddon };
}
