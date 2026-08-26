import type { FitAddon } from "@xterm/addon-fit";
import type { IDisposable, ITerminalOptions, ITheme, IWindowsPty, Terminal } from "@xterm/xterm";

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
 * macOS ships no Cascadia/Consolas/SimSun. The Windows-first stack would fall
 * back to `monospace` (Menlo) for Latin and a proportional system CJK font
 * for Chinese, which reads as a broken mixed-font grid inside the fixed-cell
 * canvas. Detect the platform so each OS gets a stack it can actually resolve.
 */
function detectMacOS(env: NavigatorLike = typeof navigator === "undefined" ? {} : navigator): boolean {
  const ua = env.userAgent ?? "";
  const platform = env.platform ?? "";
  return /Mac OS X|Macintosh/i.test(ua) || platform === "MacIntel" || platform === "MacARM";
}

/**
 * Options for hosting Claude Code (Ink) and Codex (fullscreen TUI) over a PTY.
 *
 * `convertEol` must stay false: xterm docs say PTY/termios already translates
 * `\n` → `\r\n`. Enabling it makes TUI redraws stack as a second UI.
 */
export function buildXtermOptions(env?: NavigatorLike): ITerminalOptions {
  const windowsPty = detectWindowsPty(env);
  const mac = detectMacOS(env);
  // Windows: Cascadia/Consolas have no CJK glyphs, so NSimSun/SimSun provide a
  // monospace CJK fallback (otherwise Windows substitutes proportional YaHei
  // and Ink's cursor jumps a whole row). macOS: use SF Mono/Menlo plus
  // PingFang SC / Hiragino Sans GB for CJK.
  const fontFamily = mac
    ? '"SF Mono", Menlo, Monaco, "PingFang SC", "Hiragino Sans GB", monospace'
    : "Cascadia Mono, Consolas, NSimSun, SimSun, monospace";
  return {
    convertEol: false,
    cursorBlink: false,
    cursorInactiveStyle: "outline",
    fontFamily,
    fontSize: 13,
    // macOS CJK fallbacks (PingFang/Hiragino) have taller metrics than Menlo;
    // a bit of extra line height keeps them from clipping against the cell.
    // Windows/Linux keep 1: NSimSun is a real monospace CJK font there.
    lineHeight: mac ? 1.15 : 1,
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

export const CODEX_CURSOR_PIN = "\x1b[?25l";

/** DECSCUSR blinking styles are odd: 1 block, 3 underline, 5 bar. */
export function steadyDecscusrParam(param: number): number {
  if (param <= 0) return 0;
  return param % 2 === 1 ? param + 1 : param;
}

export function decscusrCursorStyle(param: number): ITerminalOptions["cursorStyle"] | undefined {
  switch (steadyDecscusrParam(param)) {
    case 2:
      return "block";
    case 4:
      return "underline";
    case 6:
      return "bar";
    default:
      return undefined;
  }
}

function isCompleteEscape(tail: string): boolean {
  if (tail.charCodeAt(0) === 0x9b) {
    for (let i = 1; i < tail.length; i++) {
      const code = tail.charCodeAt(i);
      if (code >= 0x40 && code <= 0x7e) return true;
    }
    return false;
  }
  if (tail.length < 2) return false;
  const intro = tail[1];
  if (intro === "[") {
    for (let i = 2; i < tail.length; i++) {
      const code = tail.charCodeAt(i);
      if (code >= 0x40 && code <= 0x7e) return true;
    }
    return false;
  }
  if (intro === "]") {
    return tail.includes("\x07") || tail.includes("\x1b\\");
  }
  return true;
}

function incompleteEscapeSuffix(data: string): string {
  const esc = Math.max(data.lastIndexOf("\x1b"), data.lastIndexOf("\x9b"));
  if (esc === -1) return "";
  const tail = data.slice(esc);
  return isCompleteEscape(tail) ? "" : tail;
}

function rewritePrivateModes(body: string, flag: string): string {
  const modes = body
    .split(";")
    .filter(Boolean)
    .map(Number)
    .filter((mode) => mode !== 12 && !(flag === "h" && mode === 25));
  if (modes.length === 0) return "";
  return `\x1b[?${modes.join(";")}${flag}`;
}

function stabilizeComplete(data: string): string {
  return data
    .replace(/(?:\x1b\[|\x9b)(\d*) q/g, () => "")
    .replace(/(?:\x1b\[|\x9b)\?([\d;]*)([hl])/g, (_match, body: string, flag: string) => rewritePrivateModes(body, flag));
}

/**
 * Codex's hardware cursor sits on the prompt and blinks. Hide it; input still
 * goes to the CLI. Claude draws its own inverse caret and is left alone.
 */
export function createOutputStabilizer(): { reset(): void; push(chunk: string): string } {
  let pending = "";
  return {
    reset() {
      pending = "";
    },
    push(chunk: string) {
      const data = pending + chunk;
      const suffix = incompleteEscapeSuffix(data);
      pending = suffix;
      const complete = suffix ? data.slice(0, data.length - suffix.length) : data;
      if (!complete) return "";
      return stabilizeComplete(complete) + CODEX_CURSOR_PIN;
    },
  };
}

/** Keep the Codex hardware cursor hidden even if a show/blink sequence arrives. */
export function pinCursorSteady(terminal: Terminal): IDisposable {
  terminal.options.cursorBlink = false;

  const decset = terminal.parser.registerCsiHandler({ prefix: "?", final: "h" }, (params) => {
    const modes = params.flat();
    if (modes.includes(12) || modes.includes(25)) {
      terminal.options.cursorBlink = false;
      return modes.every((mode) => mode === 12 || mode === 25);
    }
    return false;
  });
  const decscusr = terminal.parser.registerCsiHandler({ intermediates: " ", final: "q" }, () => {
    terminal.options.cursorBlink = false;
    return true;
  });
  const parsed = terminal.onWriteParsed(() => {
    if (terminal.options.cursorBlink) terminal.options.cursorBlink = false;
  });

  return {
    dispose() {
      decset.dispose();
      decscusr.dispose();
      parsed.dispose();
    },
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
