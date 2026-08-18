import type { FitAddon } from "@xterm/addon-fit";
import type { Terminal } from "@xterm/xterm";

export interface XtermRuntime {
  Terminal: typeof Terminal;
  FitAddon: typeof FitAddon;
}

/** Load the canvas-backed renderer only after the terminal workspace opens. */
export async function loadXtermRuntime(): Promise<XtermRuntime> {
  const [{ Terminal }, { FitAddon }] = await Promise.all([
    import("@xterm/xterm"),
    import("@xterm/addon-fit"),
  ]);
  return { Terminal, FitAddon };
}
