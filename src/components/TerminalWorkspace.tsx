import { Fragment, useCallback, useEffect, useImperativeHandle, useRef, useState, type CSSProperties, type Ref } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import type { FitAddon } from "@xterm/addon-fit";
import type { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { resizeTerminal, startTerminal, stopTerminal, writeTerminal } from "../lib/terminalApi";
import { buildXtermOptions, CODEX_CURSOR_PIN, createOutputStabilizer, loadXtermRuntime, pinCursorSteady } from "../lib/xtermRuntime";
import type { TerminalAgent, TerminalSessionInfo, TerminalStatus } from "../types";
import { Icon, IconButton } from "./Icon";
import { SplitHandle } from "./SplitHandle";
import { useElementSize, useStoredNumber, useStoredString } from "../lib/layoutPreferences";

const TERMINAL_STACK_WIDTH = 720;
const TERMINAL_MIN_WIDTH = 320;
const TERMINAL_MIN_HEIGHT = 220;
const SPLITTER_SIZE = 12;

function clampRatio(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), Math.max(min, max));
}

interface PaneState {
  session: TerminalSessionInfo | null;
  status: TerminalStatus;
  error: string;
}

const initialPane = (): PaneState => ({
  session: null,
  status: "idle",
  error: "",
});

const AGENTS: { id: TerminalAgent; label: string; description: string }[] = [
  { id: "claude", label: "Claude CLI", description: "本机 Claude Code CLI" },
  { id: "codex", label: "Codex CLI", description: "本机 codex CLI" },
];

function writeIdleBanner(terminal: Terminal, agent: { label: string; description: string }) {
  terminal.writeln(`\x1b[90m${agent.label} · ${agent.description}\x1b[0m`);
  terminal.writeln("\x1b[90m点击启动后，终端会连接到本机 CLI。\x1b[0m");
}

/**
 * `listen()` is async. React StrictMode (and fast remounts) run cleanup before
 * the unlisten handle exists, which would leak a second PTY subscriber and
 * paint every chunk twice — Ink/ratatui then looks stacked.
 */
function listenWhileMounted<T>(
  event: string,
  handler: (event: { payload: T }) => void,
): () => void {
  let cancelled = false;
  let unlisten: UnlistenFn | undefined;
  void listen<T>(event, handler).then(
    (fn) => {
      if (cancelled) {
        fn();
        return;
      }
      unlisten = fn;
    },
    () => {
      // Subscribe failed; the next mount retries.
    },
  );
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

interface StartOptions {
  workDir?: string;
  args?: string[];
}

/** 供 App 命令式驱动终端（如会话列表「在终端中续聊」） */
export interface TerminalWorkspaceHandle {
  startWith: (agent: TerminalAgent, opts?: StartOptions) => void;
}

interface Props {
  active: boolean;
  onRunningChange?: (count: number) => void;
  /** 项目工作目录（Claude pane 的共享上下文，由 App 持有，监督闭环同源） */
  projectWorkDir?: string;
  onProjectWorkDirChange?: (dir: string) => void;
  ref?: Ref<TerminalWorkspaceHandle>;
}

/**
 * Two persistent local CLI panes. The browser only renders xterm; the Rust side
 * owns the PTY, child process and lifecycle so switching workspaces is harmless.
 * The Claude pane's work dir is the shared project context (App-owned); the
 * Codex pane keeps its own persisted dir for side-by-side review.
 */
export function TerminalWorkspace({ active, onRunningChange, projectWorkDir, onProjectWorkDirChange, ref }: Props) {
  const [panes, setPanes] = useState<Record<TerminalAgent, PaneState>>({
    claude: initialPane(),
    codex: initialPane(),
  });
  const panesRef = useRef(panes);
  const terminals = useRef(new Map<TerminalAgent, Terminal>());
  const inputQueuesRef = useRef<Record<TerminalAgent, Promise<void>>>({
    claude: Promise.resolve(),
    codex: Promise.resolve(),
  });
  const codexStabilizerRef = useRef(createOutputStabilizer());
  const pendingOutputRef = useRef(new Map<TerminalAgent, { sessionId: string; data: string }>());
  const outputFrameRef = useRef<number | null>(null);
  const [codexWorkDir, setCodexWorkDir] = useStoredString("ha-workdir-codex", "");
  const gridRef = useRef<HTMLDivElement>(null);
  const gridSize = useElementSize(gridRef);
  const stacked = gridSize.width > 0 && gridSize.width < TERMINAL_STACK_WIDTH;
  const [wideRatio, setWideRatio] = useStoredNumber("ha-layout-terminal-width", 50);
  const [stackedRatio, setStackedRatio] = useStoredNumber("ha-layout-terminal-height", 50);
  const axisSize = stacked ? gridSize.height : gridSize.width;
  const minimumPaneSize = stacked ? TERMINAL_MIN_HEIGHT : TERMINAL_MIN_WIDTH;
  const minRatio = axisSize > 0
    ? Math.min(50, Math.max(20, (minimumPaneSize / Math.max(1, axisSize - SPLITTER_SIZE)) * 100))
    : 30;
  const maxRatio = 100 - minRatio;
  const ratio = clampRatio(stacked ? stackedRatio : wideRatio, minRatio, maxRatio);
  const setRatio = stacked ? setStackedRatio : setWideRatio;

  // panesRef 以 ref 为准、同步更新：terminal-exit 事件与 stop invoke 的拒绝
  // 可能在同一 tick 内先后到达，若靠 useEffect 在提交后才同步 ref，catch 里
  // 读到的是旧值（session 仍在），竞态判断失效（M4 卡死的真实根因）
  const patchPane = useCallback((agent: TerminalAgent, patch: Partial<PaneState>) => {
    const current = panesRef.current;
    const next = { ...current, [agent]: { ...current[agent], ...patch } };
    panesRef.current = next;
    setPanes(next);
  }, []);

  const mountTerminal = useCallback((agent: TerminalAgent, terminal: Terminal) => {
    terminals.current.set(agent, terminal);
  }, []);

  const unmountTerminal = useCallback((agent: TerminalAgent) => {
    terminals.current.delete(agent);
  }, []);

  const flushOutput = useCallback(() => {
    outputFrameRef.current = null;
    const output = Array.from(pendingOutputRef.current.entries());
    pendingOutputRef.current.clear();
    for (const [agent, chunk] of output) {
      if (panesRef.current[agent].session?.id === chunk.sessionId) {
        const data = agent === "codex" ? codexStabilizerRef.current.push(chunk.data) : chunk.data;
        if (data) terminals.current.get(agent)?.write(data);
      }
    }
  }, []);

  const enqueueOutput = useCallback((agent: TerminalAgent, sessionId: string, data: string) => {
    const pending = pendingOutputRef.current.get(agent);
    if (pending?.sessionId === sessionId) {
      pending.data += data;
    } else {
      pendingOutputRef.current.set(agent, { sessionId, data });
    }
    if (outputFrameRef.current !== null) return;
    outputFrameRef.current = -1;
    const frame = window.requestAnimationFrame(flushOutput);
    if (outputFrameRef.current === -1) outputFrameRef.current = frame;
  }, [flushOutput]);

  useEffect(() => () => {
    if (outputFrameRef.current !== null && outputFrameRef.current >= 0) {
      window.cancelAnimationFrame(outputFrameRef.current);
    }
    outputFrameRef.current = null;
    pendingOutputRef.current.clear();
  }, []);

  const runningCount = Object.values(panes).filter((pane) => pane.status === "running").length;

  useEffect(() => {
    onRunningChange?.(runningCount);
  }, [onRunningChange, runningCount]);

  // PTY output is byte-oriented; never split it by line because ANSI cursor
  // sequences and partial UTF-8 chunks may span multiple events.
  useEffect(() => {
    const stopOutput = listenWhileMounted<{ sessionId: string; data: string }>(
      "terminal-output",
      (event) => {
        for (const [agent] of terminals.current.entries()) {
          if (panesRef.current[agent].session?.id === event.payload.sessionId) {
            enqueueOutput(agent, event.payload.sessionId, event.payload.data);
            break;
          }
        }
      },
    );
    const stopExit = listenWhileMounted<{ sessionId: string; code?: number | null }>(
      "terminal-exit",
      (event) => {
        for (const agent of ["claude", "codex"] as TerminalAgent[]) {
          if (panesRef.current[agent].session?.id === event.payload.sessionId) {
            const code = event.payload.code;
            patchPane(agent, {
              status: "exited",
              session: null,
              error: code != null && code !== 0 ? `CLI 异常退出（代码 ${code}）` : "",
            });
            break;
          }
        }
      },
    );
    const stopError = listenWhileMounted<{ sessionId?: string; message: string }>(
      "terminal-error",
      (event) => {
        for (const agent of ["claude", "codex"] as TerminalAgent[]) {
          if (!event.payload.sessionId || panesRef.current[agent].session?.id === event.payload.sessionId) {
            patchPane(agent, { status: "error", error: event.payload.message });
            if (event.payload.sessionId) break;
          }
        }
      },
    );

    return () => {
      stopOutput();
      stopExit();
      stopError();
    };
  }, [enqueueOutput, patchPane]);

  const handleStart = useCallback(
    async (agent: TerminalAgent, cols: number, rows: number, opts?: StartOptions) => {
      const pane = panesRef.current[agent];
      if (["starting", "running", "stopping"].includes(pane.status)) return;
      const workDir = (opts?.workDir ?? (agent === "claude" ? projectWorkDir ?? "" : codexWorkDir)).trim();
      if (!workDir) {
        patchPane(agent, { status: "error", error: "请输入工作目录" });
        return;
      }
      patchPane(agent, { status: "starting", error: "" });
      if (agent === "codex") codexStabilizerRef.current.reset();
      try {
        const session = await startTerminal({
          agent,
          work_dir: workDir,
          cols,
          rows,
          args: opts?.args,
        });
        patchPane(agent, { session, status: "running", error: "" });
      } catch (error) {
        patchPane(agent, { status: "error", error: String(error) });
      }
    },
    [patchPane, projectWorkDir, codexWorkDir],
  );

  // 命令式启动入口（会话列表「在终端中续聊」）：目录覆盖走正常受控链路
  useImperativeHandle(
    ref,
    () => ({
      startWith: (agent, opts) => {
        if (opts?.workDir !== undefined) {
          if (agent === "claude") onProjectWorkDirChange?.(opts.workDir);
          else setCodexWorkDir(opts.workDir);
        }
        const terminal = terminals.current.get(agent);
        void handleStart(agent, terminal?.cols ?? 120, terminal?.rows ?? 30, opts);
      },
    }),
    [handleStart, onProjectWorkDirChange],
  );

  const handleStop = useCallback(
    async (agent: TerminalAgent) => {
      const session = panesRef.current[agent].session;
      if (!session) return;
      patchPane(agent, { status: "stopping", error: "" });
      try {
        await stopTerminal(session.id);
      } catch (error) {
        // 仅当会话仍在时回滚 running：CLI 恰在点停止时自行退出的竞态下，
        // terminal-exit 已把面板置为 exited/session=null，再回滚 running 会
        // 造成"停止按钮无响应且无启动按钮"的永久卡死
        if (panesRef.current[agent].session?.id === session.id) {
          patchPane(agent, { status: "running", error: String(error) });
        }
      }
    },
    [patchPane],
  );

  const handleInput = useCallback(
    (agent: TerminalAgent, data: string) => {
      const session = panesRef.current[agent].session;
      if (!session) return;
      const sessionId = session.id;
      const write = async () => {
        if (panesRef.current[agent].session?.id !== sessionId) return;
        try {
          await writeTerminal(sessionId, data);
        } catch (error) {
          patchPane(agent, { status: "error", error: String(error) });
        }
      };
      inputQueuesRef.current[agent] = inputQueuesRef.current[agent]
        .catch(() => undefined)
        .then(write);
    },
    [patchPane],
  );

  return (
    <div className={`terminal-workspace ${active ? "is-active" : ""}`}>
      <div className="terminal-intro">
        <div>
          <span className="eyebrow">LOCAL CLI WORKBENCH</span>
          <h2>两个终端，保持上下文</h2>
          <p>按需启动本机已安装的 Claude Code 和 Codex CLI。切换工作区不会结束进程。</p>
        </div>
        <div className="terminal-intro__note">
          <Icon name="shield" size={15} />
          <span>仅调用本机 CLI，不读取或托管凭据</span>
        </div>
      </div>
      <div
        className={`terminal-grid ${stacked ? "terminal-grid--stacked" : ""}`}
        ref={gridRef}
        style={{ "--terminal-primary-size": `${ratio}%` } as CSSProperties}
      >
        {AGENTS.map((agent, index) => (
          <Fragment key={agent.id}>
            {index > 0 && (
              <SplitHandle
                orientation={stacked ? "horizontal" : "vertical"}
                label="调整 Claude 与 Codex 终端区域"
                value={ratio}
                min={minRatio}
                max={maxRatio}
                onChange={setRatio}
                pixelsPerUnit={Math.max(1, axisSize - SPLITTER_SIZE) / 100}
                step={5}
                className="terminal-split"
                valueText={`Claude 终端区域占比 ${Math.round(ratio)}%`}
              />
            )}
            <TerminalPane
              agent={agent}
              active={active}
              pane={panes[agent.id]}
              workDir={agent.id === "claude" ? (projectWorkDir ?? "") : codexWorkDir}
              onMount={(terminal) => mountTerminal(agent.id, terminal)}
              onUnmount={() => unmountTerminal(agent.id)}
              onStart={handleStart}
              onStop={handleStop}
              onInput={handleInput}
              onResize={async (sessionId, cols, rows) => {
                try {
                  await resizeTerminal(sessionId, cols, rows);
                } catch (error) {
                  patchPane(agent.id, { error: String(error) });
                }
              }}
              onWorkDirChange={(workDir) => {
                if (agent.id === "claude") onProjectWorkDirChange?.(workDir);
                else setCodexWorkDir(workDir);
              }}
            />
          </Fragment>
        ))}
      </div>
    </div>
  );
}

interface PaneProps {
  agent: { id: TerminalAgent; label: string; description: string };
  active: boolean;
  pane: PaneState;
  workDir: string;
  onMount: (terminal: Terminal) => void;
  onUnmount: () => void;
  onStart: (agent: TerminalAgent, cols: number, rows: number) => void;
  onStop: (agent: TerminalAgent) => void;
  onInput: (agent: TerminalAgent, data: string) => void;
  onResize: (sessionId: string, cols: number, rows: number) => void;
  onWorkDirChange: (workDir: string) => void;
}

function TerminalPane({
  agent,
  active,
  pane,
  workDir,
  onMount,
  onUnmount,
  onStart,
  onStop,
  onInput,
  onResize,
  onWorkDirChange,
}: PaneProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const sessionRef = useRef(pane.session);
  const activeRef = useRef(active);
  const mountingRef = useRef(false);
  const mountedRef = useRef(false);
  const unmountedRef = useRef(false);
  const dataListenerRef = useRef<{ dispose: () => void } | null>(null);
  const cursorPinRef = useRef<{ dispose: () => void } | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const fitFrameRef = useRef<number | null>(null);
  const lastPtySizeRef = useRef("");
  const callbacksRef = useRef({ onMount, onUnmount, onInput, onResize });
  const [terminalReady, setTerminalReady] = useState(false);
  const [terminalError, setTerminalError] = useState("");

  useEffect(() => {
    sessionRef.current = pane.session;
    const terminal = terminalRef.current;
    // Spawn already used the current cols/rows. A same-size ConPTY resize still
    // SIGWINCH Ink, which commits the welcome screen to Static and draws it again.
    if (pane.session && terminal) {
      lastPtySizeRef.current = `${pane.session.id}:${terminal.cols}x${terminal.rows}`;
    } else if (!pane.session) {
      lastPtySizeRef.current = "";
    }
  }, [pane.session]);

  useEffect(() => {
    activeRef.current = active;
  }, [active]);

  useEffect(() => {
    callbacksRef.current = { onMount, onUnmount, onInput, onResize };
  }, [onInput, onMount, onResize, onUnmount]);

  const scheduleFitAndResize = useCallback(() => {
    if (!activeRef.current || fitFrameRef.current !== null) return;
    fitFrameRef.current = -1;
    const run = () => {
      fitFrameRef.current = null;
      if (!activeRef.current) return;
      const terminal = terminalRef.current;
      if (!terminal) return;
      try {
        fitRef.current?.fit();
        const session = sessionRef.current;
        const sizeKey = session ? `${session.id}:${terminal.cols}x${terminal.rows}` : "";
        if (session && lastPtySizeRef.current !== sizeKey) {
          lastPtySizeRef.current = sizeKey;
          callbacksRef.current.onResize(session.id, terminal.cols, terminal.rows);
        }
      } catch {
        // A hidden or detached terminal can be fitted on the next activation.
      }
    };
    const frame = window.requestAnimationFrame(run);
    if (fitFrameRef.current === -1) fitFrameRef.current = frame;
  }, []);

  const disposeTerminal = useCallback(() => {
    dataListenerRef.current?.dispose();
    dataListenerRef.current = null;
    cursorPinRef.current?.dispose();
    cursorPinRef.current = null;
    resizeObserverRef.current?.disconnect();
    resizeObserverRef.current = null;
    if (fitFrameRef.current !== null && fitFrameRef.current >= 0) {
      window.cancelAnimationFrame(fitFrameRef.current);
    }
    fitFrameRef.current = null;
    terminalRef.current?.dispose();
    terminalRef.current = null;
    fitRef.current = null;
    setTerminalReady(false);
    if (mountedRef.current) {
      mountedRef.current = false;
      callbacksRef.current.onUnmount();
    }
  }, []);

  // xterm is deliberately imported only after this workspace is opened. That
  // keeps the initial application bundle lighter and avoids canvas setup while
  // users are browsing transcripts, supervision, or MCP status.
  useEffect(() => {
    if (!active || !hostRef.current || terminalRef.current || mountingRef.current) return;
    mountingRef.current = true;

    void loadXtermRuntime()
      .then(({ Terminal, FitAddon }) => {
        if (unmountedRef.current || !hostRef.current || terminalRef.current) return;
        const terminal = new Terminal(buildXtermOptions());
        const fit = new FitAddon();
        terminal.loadAddon(fit);
        terminal.open(hostRef.current);
        fitRef.current = fit;
        terminalRef.current = terminal;
        try {
          fit.fit();
        } catch {
          // Cell metrics can still be 0; scheduleFitAndResize retries.
        }
        callbacksRef.current.onMount(terminal);
        mountedRef.current = true;
        setTerminalReady(true);
        writeIdleBanner(terminal, agent);
        dataListenerRef.current = terminal.onData((data) => callbacksRef.current.onInput(agent.id, data));
        if (agent.id === "codex") {
          cursorPinRef.current = pinCursorSteady(terminal);
          terminal.write(CODEX_CURSOR_PIN);
        }
        // FitAddon no-ops while cell metrics are 0; retry after layout.
        scheduleFitAndResize();
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            if (!unmountedRef.current) scheduleFitAndResize();
          });
        });

        if (typeof ResizeObserver !== "undefined") {
          const observer = new ResizeObserver(() => {
            scheduleFitAndResize();
          });
          observer.observe(hostRef.current);
          resizeObserverRef.current = observer;
        }
      })
      .catch((error) => {
        if (!unmountedRef.current) setTerminalError(`终端渲染器加载失败: ${String(error)}`);
      })
      .finally(() => {
        mountingRef.current = false;
      });
  }, [active, agent.description, agent.id, agent.label, scheduleFitAndResize]);

  // Changing workspaces hides the pane but intentionally does not dispose it.
  // Refit after it becomes visible so the native PTY receives accurate geometry.
  useEffect(() => {
    if (!active || !terminalRef.current) return;
    scheduleFitAndResize();
  }, [active, pane.session, scheduleFitAndResize]);

  // Drop the idle banner before the TUI takes the alternate screen. Leaving
  // those lines in the buffer makes Ink/ratatui look like a stacked second UI.
  // If start fails before a session exists, put the banner back so the pane
  // is not a blank black box. Do not reset on `exited`: the last TUI frame is useful.
  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    if (pane.status === "starting") {
      terminal.reset();
      return;
    }
    if (pane.status === "error" && !pane.session) {
      writeIdleBanner(terminal, agent);
    }
  }, [agent, pane.session, pane.status]);

  useEffect(() => {
    unmountedRef.current = false;
    return () => {
      unmountedRef.current = true;
      disposeTerminal();
    };
  }, [disposeTerminal]);

  const browseWorkDir = async () => {
    try {
      const dir = await open({
        directory: true,
        multiple: false,
        title: `选择 ${agent.label} 工作目录`,
        defaultPath: workDir.trim() || undefined,
      });
      if (typeof dir === "string") onWorkDirChange(dir);
    } catch {
      // Keep the current path if the dialog fails or is unavailable.
    }
  };

  const statusLabel = {
    idle: "未启动",
    starting: "启动中",
    running: "运行中",
    stopping: "停止中",
    exited: "已退出",
    error: "需要处理",
  }[pane.status];

  return (
    <article className={`terminal-pane terminal-pane--${agent.id}`}>
      <header className="terminal-pane__head">
        <div className="terminal-pane__identity">
          <span className={`agent-avatar agent-avatar--${agent.id}`}>
            <Icon name={agent.id === "claude" ? "spark" : "code"} size={15} />
          </span>
          <div>
            <strong>{agent.label}</strong>
            <span>{agent.description}</span>
          </div>
        </div>
        <span className={`terminal-status terminal-status--${pane.status}`}>
          <span className="status-dot" /> {statusLabel}
        </span>
      </header>
      <div className="terminal-pane__toolbar">
        <IconButton
          label={`选择 ${agent.label} 工作目录`}
          className="icon-button--toolbar"
          onClick={() => void browseWorkDir()}
        >
          <Icon name="folder" size={13} />
        </IconButton>
        <input
          value={workDir}
          onChange={(event) => onWorkDirChange(event.currentTarget.value)}
          aria-label={`${agent.label} 工作目录`}
          title="工作目录（也可点左侧文件夹图标选择）"
          spellCheck={false}
        />
        {pane.status === "running" || pane.status === "stopping" ? (
          <IconButton
            label={`停止 ${agent.label}`}
            className="icon-button--danger"
            onClick={() => onStop(agent.id)}
            disabled={pane.status === "stopping"}
          >
            <Icon name="stop" size={15} />
          </IconButton>
        ) : (
          <button
            type="button"
            className="button button--primary button--compact"
            onClick={() => {
              try {
                fitRef.current?.fit();
              } catch {
                // Fit can throw while the pane is still collapsing; start with last known size.
              }
              const terminal = terminalRef.current;
              onStart(agent.id, terminal?.cols ?? 120, terminal?.rows ?? 30);
            }}
            disabled={pane.status === "starting" || !terminalReady}
          >
            <Icon name="play" size={14} /> {pane.status === "starting" ? "启动中" : "启动"}
          </button>
        )}
      </div>
      <div ref={hostRef} className="terminal-surface" aria-label={`${agent.label} 终端输出`} />
      {(pane.error || terminalError) && (
        <div className="terminal-pane__error" role="alert">
          <Icon name="alert" size={14} />
          <span>{pane.error || terminalError}</span>
        </div>
      )}
      <div className="terminal-pane__foot">
        <span><Icon name="keyboard" size={13} /> 输入由本机 CLI 处理</span>
        <span>{pane.session?.id ?? "等待 PTY"}</span>
      </div>
    </article>
  );
}
