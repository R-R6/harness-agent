import { useCallback, useEffect, useImperativeHandle, useRef, useState, type CSSProperties, type Ref } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { FitAddon } from "@xterm/addon-fit";
import type { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { resizeTerminal, startTerminal, stopTerminal, writeTerminal } from "../lib/terminalApi";
import { buildXtermOptions, CODEX_CURSOR_PIN, createOutputStabilizer, loadXtermRuntime, pinCursorSteady } from "../lib/xtermRuntime";
import { listenWhileMounted } from "../lib/listenWhileMounted";
import { fetchAgentCatalog } from "../lib/api";
import type { AgentCatalogEntry, TerminalAgent, TerminalSessionInfo, TerminalStatus } from "../types";
import { basenameOf, samePath } from "../lib/workspaces";
import { Icon, IconButton } from "./Icon";
import { SplitHandle } from "./SplitHandle";
import { useElementSize, useStoredNumber, useStoredString } from "../lib/layoutPreferences";
const TERMINAL_STACK_WIDTH = 720;
const TERMINAL_MIN_WIDTH = 320;
const TERMINAL_MIN_HEIGHT = 220;
const SPLITTER_SIZE = 12;
const CODEX_KEY = "codex";

function clampRatio(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), Math.max(min, max));
}

function isBusyStatus(status: TerminalStatus) {
  return status === "starting" || status === "running" || status === "stopping";
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

/** 退出码 → 面板错误文案；0 / 主动停止(null) 不视为异常 */
function exitErrorMessage(code?: number | null) {
  return code != null && code !== 0 ? `CLI 异常退出（代码 ${code}）` : "";
}

interface ClaudeTab {
  id: string;
  /** 该 tab 绑定的 CLI Agent（注册表 id；Claude 列已泛化为工人列） */
  agentId: string;
  pane: PaneState;
}

const AGENTS: { id: TerminalAgent; label: string; description: string }[] = [
  { id: "claude", label: "Claude CLI", description: "本机 Claude Code CLI" },
  { id: "codex", label: "Codex CLI", description: "本机 codex CLI" },
];

const CODEX_META = AGENTS[1];

function writeIdleBanner(terminal: Terminal, agent: { label: string; description: string }) {
  terminal.writeln(`\x1b[90m${agent.label} · ${agent.description}\x1b[0m`);
  terminal.writeln("\x1b[90m点击启动后，终端会连接到本机 CLI。\x1b[0m");
}

interface StartOptions {
  workDir?: string;
  args?: string[];
}

/** 供 App 命令式驱动终端（如会话列表「在终端中续聊」/ 监督驱动） */
export interface TerminalWorkspaceHandle {
  startWith: (agent: TerminalAgent, opts?: StartOptions) => void;
  /** 始终新建 Claude PTY（驱动任务用），返回 session id */
  startClaudeForTask: (workDir?: string) => Promise<string>;
  /** 找到指定 Agent 在该目录下的空闲已启动 pane，返回 session id（多 Agent 驱动用） */
  claimIdleAgentPane: (agentId: string, workDir: string) => string | null;
  focusSession: (sessionId: string) => void;
  /** 把键盘焦点交给当前 Claude xterm（切到终端 tab 后收键） */
  focusActiveClaude: () => void;
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
 * Claude 可多开（一任务一 PTY）；Codex 保持单 pane。
 * 浏览器只渲染 xterm；Rust 侧拥有 PTY / 子进程。
 */
export function TerminalWorkspace({ active, onRunningChange, projectWorkDir, onProjectWorkDirChange, ref }: Props) {
  const claudeSeqRef = useRef(1);
  const [claudeTabs, setClaudeTabs] = useState<ClaudeTab[]>(() => [
    { id: "claude-1", agentId: "claude", pane: initialPane() },
  ]);
  /** Agent 注册表状态（工作台启动条 + tab 标题），挂载时拉取一次 */
  const [catalog, setCatalog] = useState<AgentCatalogEntry[]>([]);  const [activeClaudeId, setActiveClaudeId] = useState("claude-1");
  const [codexPane, setCodexPane] = useState<PaneState>(initialPane);
  /** 后端通知横幅（预信任失败等：不致命，但用户必须在启动终端前看见） */
  const [terminalNotice, setTerminalNotice] = useState("");
  const claudeTabsRef = useRef(claudeTabs);
  const activeClaudeIdRef = useRef(activeClaudeId);
  const codexPaneRef = useRef(codexPane);
  const terminals = useRef(new Map<string, Terminal>());
  const inputQueuesRef = useRef(new Map<string, Promise<void>>());
  const codexStabilizerRef = useRef(createOutputStabilizer());
  const noticeTimerRef = useRef<number | null>(null);
  const pendingOutputRef = useRef(new Map<string, { sessionId: string; data: string }>());
  /** sessionId → 尚未绑定到 pane 的输出（start_terminal 返回前 PTY 已可能吐字） */
  const orphanOutputRef = useRef(new Map<string, string>());
  /** sessionId → 早于 pane 绑定到达的退出事件（CLI 秒退/崩溃时 terminal-exit 先于 invoke 返回） */
  const orphanExitRef = useRef(new Map<string, { code?: number | null }>());
  const outputFrameRef = useRef<number | null>(null);
  const claudeStartChainRef = useRef(Promise.resolve());
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

  const syncClaudeTabs = useCallback((next: ClaudeTab[]) => {
    claudeTabsRef.current = next;
    setClaudeTabs(next);
  }, []);

  const patchClaudeTab = useCallback((tabId: string, patch: Partial<PaneState>) => {
    const next = claudeTabsRef.current.map((tab) =>
      tab.id === tabId ? { ...tab, pane: { ...tab.pane, ...patch } } : tab,
    );
    syncClaudeTabs(next);
  }, [syncClaudeTabs]);

  const patchCodex = useCallback((patch: Partial<PaneState>) => {
    const next = { ...codexPaneRef.current, ...patch };
    codexPaneRef.current = next;
    setCodexPane(next);
  }, []);

  /** tab 标题/横幅用 meta：静态表优先，注册表条目兜底（新 Agent 未适配静态文案时） */
  const agentMetaFor = useCallback(
    (agentId: string): { id: TerminalAgent; label: string; description: string } => {
      const known = AGENTS.find((a) => a.id === agentId);
      if (known) return known;
      const entry = catalog.find((c) => c.id === agentId);
      if (entry) {
        return { id: agentId, label: entry.name, description: `${entry.name} · 本机 CLI` };
      }
      return { id: agentId, label: agentId, description: "本机 CLI" };
    },
    [catalog],
  );

  const addClaudeTab = useCallback((agentId: string = "claude") => {
    claudeSeqRef.current += 1;
    const id = `claude-${claudeSeqRef.current}`;
    const next = [...claudeTabsRef.current, { id, agentId, pane: initialPane() }];
    syncClaudeTabs(next);
    activeClaudeIdRef.current = id;
    setActiveClaudeId(id);
    return id;
  }, [syncClaudeTabs]);

  const focusClaudeTab = useCallback((tabId: string) => {
    activeClaudeIdRef.current = tabId;
    setActiveClaudeId(tabId);
  }, []);

  const unmountTerminal = useCallback((paneKey: string) => {
    terminals.current.delete(paneKey);
  }, []);

  const flushOutput = useCallback(() => {
    outputFrameRef.current = null;
    const output = Array.from(pendingOutputRef.current.entries());
    pendingOutputRef.current.clear();
    let deferred = false;
    for (const [paneKey, chunk] of output) {
      const pane =
        paneKey === CODEX_KEY
          ? codexPaneRef.current
          : claudeTabsRef.current.find((t) => t.id === paneKey)?.pane;
      if (pane?.session?.id !== chunk.sessionId) continue;
      const term = terminals.current.get(paneKey);
      if (!term) {
        // xterm 尚未 mount：放回队列，等 onMount 再刷，避免启动瞬间黑屏丢字
        const existing = pendingOutputRef.current.get(paneKey);
        if (existing?.sessionId === chunk.sessionId) existing.data += chunk.data;
        else pendingOutputRef.current.set(paneKey, chunk);
        deferred = true;
        continue;
      }
      const data = paneKey === CODEX_KEY ? codexStabilizerRef.current.push(chunk.data) : chunk.data;
      if (data) term.write(data);
    }
    if (deferred && outputFrameRef.current === null) {
      outputFrameRef.current = -1;
      const frame = window.requestAnimationFrame(flushOutput);
      if (outputFrameRef.current === -1) outputFrameRef.current = frame;
    }
  }, []);

  const enqueueOutput = useCallback((paneKey: string, sessionId: string, data: string) => {
    const pending = pendingOutputRef.current.get(paneKey);
    if (pending?.sessionId === sessionId) {
      pending.data += data;
    } else {
      pendingOutputRef.current.set(paneKey, { sessionId, data });
    }
    if (outputFrameRef.current !== null) return;
    outputFrameRef.current = -1;
    const frame = window.requestAnimationFrame(flushOutput);
    if (outputFrameRef.current === -1) outputFrameRef.current = frame;
  }, [flushOutput]);

  const claimOrphanOutput = useCallback((paneKey: string, sessionId: string) => {
    const buffered = orphanOutputRef.current.get(sessionId);
    if (!buffered) return;
    orphanOutputRef.current.delete(sessionId);
    const term = terminals.current.get(paneKey);
    // 同步写入：避免 rAF 晚于 starting→reset 把欢迎屏清掉
    if (term) {
      const data = paneKey === CODEX_KEY ? codexStabilizerRef.current.push(buffered) : buffered;
      if (data) term.write(data);
      return;
    }
    enqueueOutput(paneKey, sessionId, buffered);
  }, [enqueueOutput]);

  const mountTerminal = useCallback((paneKey: string, terminal: Terminal) => {
    terminals.current.set(paneKey, terminal);
    // mount 晚于 PTY 输出时，把积压刷进刚就绪的 xterm
    if (outputFrameRef.current === null && pendingOutputRef.current.size > 0) {
      outputFrameRef.current = -1;
      const frame = window.requestAnimationFrame(flushOutput);
      if (outputFrameRef.current === -1) outputFrameRef.current = frame;
    }
  }, [flushOutput]);

  useEffect(() => () => {
    if (outputFrameRef.current !== null && outputFrameRef.current >= 0) {
      window.cancelAnimationFrame(outputFrameRef.current);
    }
    outputFrameRef.current = null;
    pendingOutputRef.current.clear();
    orphanOutputRef.current.clear();
    orphanExitRef.current.clear();
  }, []);

  const runningCount =
    claudeTabs.filter((tab) => tab.pane.status === "running").length +
    (codexPane.status === "running" ? 1 : 0);

  useEffect(() => {
    onRunningChange?.(runningCount);
  }, [onRunningChange, runningCount]);

  const findPaneKeyBySession = useCallback((sessionId: string): string | null => {
    for (const tab of claudeTabsRef.current) {
      if (tab.pane.session?.id === sessionId) return tab.id;
    }
    if (codexPaneRef.current.session?.id === sessionId) return CODEX_KEY;
    return null;
  }, []);

  useEffect(() => {
    const stopOutput = listenWhileMounted<{ sessionId: string; data: string }>(
      "terminal-output",
      (event) => {
        const paneKey = findPaneKeyBySession(event.payload.sessionId);
        if (paneKey) {
          enqueueOutput(paneKey, event.payload.sessionId, event.payload.data);
          return;
        }
        const prev = orphanOutputRef.current.get(event.payload.sessionId) ?? "";
        orphanOutputRef.current.set(event.payload.sessionId, prev + event.payload.data);
      },
    );
    const stopExit = listenWhileMounted<{ sessionId: string; code?: number | null }>(
      "terminal-exit",
      (event) => {
        const { sessionId, code } = event.payload;
        const paneKey = findPaneKeyBySession(sessionId);
        if (!paneKey) {
          // CLI 在 start_terminal 返回、session 绑定到 pane 之前就退出了（例如二进制
          // 秒退/崩溃）。此时会话尚未写入 tabs/pane，直接丢弃会永久卡在"运行中"：
          // 先缓冲，等 startPane 拿到 session 后立即应用。
          orphanExitRef.current.set(sessionId, { code });
          return;
        }
        orphanExitRef.current.delete(sessionId);
        const patch = {
          status: "exited" as const,
          session: null,
          error: exitErrorMessage(code),
        };
        if (paneKey === CODEX_KEY) patchCodex(patch);
        else patchClaudeTab(paneKey, patch);
      },
    );
    const stopError = listenWhileMounted<{ sessionId?: string; message: string }>(
      "terminal-error",
      (event) => {
        if (event.payload.sessionId) {
          const paneKey = findPaneKeyBySession(event.payload.sessionId);
          if (!paneKey) return;
          if (paneKey === CODEX_KEY) patchCodex({ status: "error", error: event.payload.message });
          else patchClaudeTab(paneKey, { status: "error", error: event.payload.message });
          return;
        }
        for (const tab of claudeTabsRef.current) {
          patchClaudeTab(tab.id, { status: "error", error: event.payload.message });
        }
        patchCodex({ status: "error", error: event.payload.message });
      },
    );

    const stopNotice = listenWhileMounted<{ message: string }>("terminal-notice", (event) => {
      setTerminalNotice(event.payload.message);
      if (noticeTimerRef.current) window.clearTimeout(noticeTimerRef.current);
      noticeTimerRef.current = window.setTimeout(() => setTerminalNotice(""), 15_000);
    });

    return () => {
      stopOutput();
      stopExit();
      stopError();
      stopNotice();
    };
  }, [enqueueOutput, findPaneKeyBySession, patchClaudeTab, patchCodex]);

  // Agent 注册表：挂载拉取一次（工作台启动条 + 工人 tab 标题）。
  // Promise.resolve + Array.isArray 防御：invoke 在测试桩/异常环境可能返回非 promise 或 undefined
  useEffect(() => {
    let alive = true;
    Promise.resolve(fetchAgentCatalog())
      .then((entries) => {
        if (alive && Array.isArray(entries)) setCatalog(entries);
      })
      .catch(() => {
        // 注册表拉取失败不影响既有 claude/codex 使用（走静态 AGENTS 兜底）
      });
    return () => {
      alive = false;
    };
  }, []);

  const startPane = useCallback(
    async (
      paneKey: string,
      agent: TerminalAgent,
      cols: number,
      rows: number,
      opts?: StartOptions,
      options?: { skipBusyGate?: boolean },
    ): Promise<string | null> => {
      const pane =
        paneKey === CODEX_KEY
          ? codexPaneRef.current
          : claudeTabsRef.current.find((t) => t.id === paneKey)?.pane;
      if (!pane) return null;
      if (!options?.skipBusyGate && isBusyStatus(pane.status)) {
        const patch = { error: "终端已在运行：请先停止当前会话再续聊/启动" };
        if (paneKey === CODEX_KEY) patchCodex(patch);
        else patchClaudeTab(paneKey, patch);
        return null;
      }
      const workDir = (opts?.workDir ?? (agent === "claude" ? projectWorkDir ?? "" : codexWorkDir)).trim();
      if (!workDir) {
        const patch = { status: "error" as const, error: "请输入工作目录" };
        if (paneKey === CODEX_KEY) patchCodex(patch);
        else patchClaudeTab(paneKey, patch);
        return null;
      }
      if (paneKey === CODEX_KEY) {
        patchCodex({ status: "starting", error: "" });
        codexStabilizerRef.current.reset();
      } else {
        patchClaudeTab(paneKey, { status: "starting", error: "" });
      }
      // 同步清屏，避免 useEffect(reset) 在 orphan 回放之后才跑把画面清空
      terminals.current.get(paneKey)?.reset();
      try {
        const session = await startTerminal({
          agent,
          work_dir: workDir,
          cols: Math.max(cols, 80),
          rows: Math.max(rows, 24),
          args: opts?.args,
        });
        const orphanExit = orphanExitRef.current.get(session.id);
        if (orphanExit) {
          orphanExitRef.current.delete(session.id);
          // 退出前若有残余输出也一并回放，避免"已退出但缺最后一帧"
          claimOrphanOutput(paneKey, session.id);
          const patch = {
            status: "exited" as const,
            session: null,
            error: exitErrorMessage(orphanExit.code),
          };
          if (paneKey === CODEX_KEY) patchCodex(patch);
          else patchClaudeTab(paneKey, patch);
          return null;
        }
        if (paneKey === CODEX_KEY) patchCodex({ session, status: "running", error: "" });
        else patchClaudeTab(paneKey, { session, status: "running", error: "" });
        claimOrphanOutput(paneKey, session.id);
        const term = terminals.current.get(paneKey);
        if (term?.cols && term.rows) {
          try {
            await resizeTerminal(session.id, term.cols, term.rows);
          } catch {
            // 尺寸同步失败不阻断启动；后续 ResizeObserver 会再试
          }
        }
        return session.id;
      } catch (error) {
        const patch = { status: "error" as const, error: String(error) };
        if (paneKey === CODEX_KEY) patchCodex(patch);
        else patchClaudeTab(paneKey, patch);
        return null;
      }
    },
    [claimOrphanOutput, patchClaudeTab, patchCodex, projectWorkDir, codexWorkDir],
  );

  const pickClaudeTabForResume = useCallback(() => {
    const activeId = activeClaudeIdRef.current;
    const activeTab = claudeTabsRef.current.find((t) => t.id === activeId);
    if (activeTab && !isBusyStatus(activeTab.pane.status)) return activeTab.id;
    const idle = claudeTabsRef.current.find((t) => !isBusyStatus(t.pane.status));
    if (idle) {
      focusClaudeTab(idle.id);
      return idle.id;
    }
    return addClaudeTab();
  }, [addClaudeTab, focusClaudeTab]);

  useImperativeHandle(
    ref,
    () => ({
      startWith: (agent, opts) => {
        if (opts?.workDir !== undefined) {
          if (agent === "claude") onProjectWorkDirChange?.(opts.workDir);
          else setCodexWorkDir(opts.workDir);
        }
        if (agent === "codex") {
          const terminal = terminals.current.get(CODEX_KEY);
          void startPane(CODEX_KEY, "codex", terminal?.cols ?? 120, terminal?.rows ?? 30, opts);
          return;
        }
        const tabId = pickClaudeTabForResume();
        const terminal = terminals.current.get(tabId);
        void startPane(tabId, "claude", terminal?.cols ?? 120, terminal?.rows ?? 30, opts);
      },
      startClaudeForTask: (workDir) => {
        const run = async () => {
          if (workDir !== undefined) onProjectWorkDirChange?.(workDir);
          const target = (workDir ?? projectWorkDir ?? "").trim();
          // 驱动任务必须换新 PTY：同目录里已经冻在信任菜单上的会话不会自己恢复。
          const stale = claudeTabsRef.current.filter((tab) => {
            const dir = tab.pane.session?.work_dir ?? "";
            return Boolean(target) && isBusyStatus(tab.pane.status) && dir && samePath(dir, target);
          });
          for (const tab of stale) {
            const sessionId = tab.pane.session?.id;
            if (!sessionId) continue;
            patchClaudeTab(tab.id, { status: "stopping", error: "" });
            try {
              await stopTerminal(sessionId);
            } catch {
              // 旧进程杀不掉也不阻断新启动；新 pane 才是可交互的那一个。
            }
            patchClaudeTab(tab.id, { status: "exited", session: null, error: "" });
          }
          const idle = claudeTabsRef.current.find(
            (t) => !isBusyStatus(t.pane.status) && !t.pane.session,
          );
          const tabId = idle?.id ?? addClaudeTab();
          focusClaudeTab(tabId);
          if (idle) patchClaudeTab(idle.id, { status: "starting", error: "" });
          const terminal = terminals.current.get(tabId);
          const sessionId = await startPane(
            tabId,
            "claude",
            terminal?.cols ?? 120,
            terminal?.rows ?? 30,
            { workDir },
            { skipBusyGate: true },
          );
          if (!sessionId) throw new Error("无法启动 Claude 终端");
          return sessionId;
        };
        const queued = claudeStartChainRef.current.then(run, run);
        claudeStartChainRef.current = queued.then(
          () => undefined,
          () => undefined,
        );
        return queued;
      },
      focusSession: (sessionId) => {
        const tab = claudeTabsRef.current.find((t) => t.pane.session?.id === sessionId);
        if (tab) focusClaudeTab(tab.id);
      },
      focusActiveClaude: () => {
        terminals.current.get(activeClaudeIdRef.current)?.focus();
      },
      claimIdleAgentPane: (agentId, workDir) => {
        const tab = claudeTabsRef.current.find(
          (t) =>
            t.agentId === agentId &&
            t.pane.session?.id &&
            !isBusyStatus(t.pane.status) &&
            samePath(t.pane.session.work_dir ?? "", workDir.trim()),
        );
        if (!tab) return null;
        focusClaudeTab(tab.id);
        return tab.pane.session?.id ?? null;
      },
    }),
    [addClaudeTab, focusClaudeTab, onProjectWorkDirChange, patchClaudeTab, pickClaudeTabForResume, projectWorkDir, startPane],
  );

  const handleStop = useCallback(
    async (paneKey: string) => {
      const pane =
        paneKey === CODEX_KEY
          ? codexPaneRef.current
          : claudeTabsRef.current.find((t) => t.id === paneKey)?.pane;
      const session = pane?.session;
      if (!session) return;
      if (paneKey === CODEX_KEY) patchCodex({ status: "stopping", error: "" });
      else patchClaudeTab(paneKey, { status: "stopping", error: "" });
      try {
        await stopTerminal(session.id);
      } catch (error) {
        const still =
          paneKey === CODEX_KEY
            ? codexPaneRef.current.session?.id === session.id
            : claudeTabsRef.current.find((t) => t.id === paneKey)?.pane.session?.id === session.id;
        if (still) {
          const patch = { status: "running" as const, error: String(error) };
          if (paneKey === CODEX_KEY) patchCodex(patch);
          else patchClaudeTab(paneKey, patch);
        }
      }
    },
    [patchClaudeTab, patchCodex],
  );

  const handleInput = useCallback(
    (paneKey: string, data: string) => {
      const pane =
        paneKey === CODEX_KEY
          ? codexPaneRef.current
          : claudeTabsRef.current.find((t) => t.id === paneKey)?.pane;
      const session = pane?.session;
      if (!session) return;
      const sessionId = session.id;
      const write = async () => {
        const current =
          paneKey === CODEX_KEY
            ? codexPaneRef.current.session?.id
            : claudeTabsRef.current.find((t) => t.id === paneKey)?.pane.session?.id;
        if (current !== sessionId) return;
        try {
          await writeTerminal(sessionId, data);
        } catch (error) {
          const patch = { status: "error" as const, error: String(error) };
          if (paneKey === CODEX_KEY) patchCodex(patch);
          else patchClaudeTab(paneKey, patch);
        }
      };
      const prev = inputQueuesRef.current.get(paneKey) ?? Promise.resolve();
      inputQueuesRef.current.set(
        paneKey,
        prev.catch(() => undefined).then(write),
      );
    },
    [patchClaudeTab, patchCodex],
  );

  return (
    <div className={`terminal-workspace ${active ? "is-active" : ""}`}>
      <div className="terminal-intro">
        <div>
          <span className="eyebrow">LOCAL CLI WORKBENCH</span>
          <h2>本地 CLI 工作台</h2>
          <p>按需启动本机已安装的 Claude Code 和 Codex CLI。Claude 可开多个标签；切换工作区不会结束进程。</p>
        </div>
        <div className="terminal-intro__note">
          <Icon name="shield" size={15} />
          <span>仅调用本机 CLI，不读取或托管凭据</span>
        </div>
      </div>
      {terminalNotice && (
        <div className="terminal-notice" role="status">
          <Icon name="shield" size={14} />
          <span>{terminalNotice}</span>
          <button
            type="button"
            className="terminal-notice__close"
            onClick={() => setTerminalNotice("")}
            aria-label="关闭提示"
          >
            ×
          </button>
        </div>
      )}
      <div
        className={`terminal-grid ${stacked ? "terminal-grid--stacked" : ""}`}
        ref={gridRef}
        style={{ "--terminal-primary-size": `${ratio}%` } as CSSProperties}
      >
        <div className="terminal-claude-stack">
          <div className="terminal-agent-bar" role="toolbar" aria-label="Agent 启动条">
            {catalog
              .filter((c) => c.can_work)
              .map((entry) => (
                <button
                  key={entry.id}
                  type="button"
                  className={`terminal-agent-chip ${entry.installed ? "" : "terminal-agent-chip--missing"}`}
                  onClick={() => addClaudeTab(entry.id)}
                  title={
                    entry.installed
                      ? `新开一个 ${entry.name} 终端标签`
                      : `${entry.name} 未检测到安装（仍可尝试启动）`
                  }
                >
                  <Icon name={entry.id === "claude" ? "spark" : "terminal"} size={13} />
                  {entry.name}
                  {!entry.installed ? " ·未装" : entry.sessions_present ? " ·有会话" : ""}
                </button>
              ))}
          </div>
          <div className="terminal-claude-tabs" role="tablist" aria-label="CLI 终端标签">
            {claudeTabs.map((tab, index) => (
              <button
                key={tab.id}
                type="button"
                role="tab"
                aria-selected={tab.id === activeClaudeId}
                className={`terminal-claude-tab ${tab.id === activeClaudeId ? "is-active" : ""}`}
                onClick={() => focusClaudeTab(tab.id)}
              >
                {agentMetaFor(tab.agentId).label} {index + 1}
                {tab.pane.status === "running" ? " · 运行中" : ""}
              </button>
            ))}
            <button
              type="button"
              className="terminal-claude-tab terminal-claude-tab--add"
              onClick={() => addClaudeTab("claude")}
              aria-label="新增 Claude 终端"
              title="新增一个 Claude 终端会话"
            >
              <Icon name="plus" size={13} />
            </button>
          </div>
          {claudeTabs.map((tab) => (
            <div
              key={tab.id}
              className={`terminal-claude-host ${tab.id === activeClaudeId ? "is-active" : ""}`}
              hidden={tab.id !== activeClaudeId}
              style={{ display: tab.id === activeClaudeId ? "flex" : "none" }}
            >
              <TerminalPane
                agent={agentMetaFor(tab.agentId)}
                paneKey={tab.id}
                workspaceActive={active && tab.id === activeClaudeId}
                surfaceVisible={tab.id === activeClaudeId}
                pane={tab.pane}
                workDir={projectWorkDir ?? ""}
                onMount={(terminal) => mountTerminal(tab.id, terminal)}
                onUnmount={() => unmountTerminal(tab.id)}
                onStart={(_agent, cols, rows) => {
                  void startPane(tab.id, tab.agentId, cols, rows);
                }}
                onStop={() => {
                  void handleStop(tab.id);
                }}
                onInput={(_agent, data) => handleInput(tab.id, data)}
                onResize={async (sessionId, cols, rows) => {
                  try {
                    await resizeTerminal(sessionId, cols, rows);
                  } catch (error) {
                    patchClaudeTab(tab.id, { error: String(error) });
                  }
                }}
                onWorkDirChange={(workDir) => onProjectWorkDirChange?.(workDir)}
              />
            </div>
          ))}
        </div>
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
        <TerminalPane
          agent={CODEX_META}
          paneKey={CODEX_KEY}
          workspaceActive={active}
          surfaceVisible
          pane={codexPane}
          workDir={codexWorkDir}
          onMount={(terminal) => mountTerminal(CODEX_KEY, terminal)}
          onUnmount={() => unmountTerminal(CODEX_KEY)}
          onStart={(_agent, cols, rows) => {
            void startPane(CODEX_KEY, "codex", cols, rows);
          }}
          onStop={() => {
            void handleStop(CODEX_KEY);
          }}
          onInput={(_agent, data) => handleInput(CODEX_KEY, data)}
          onResize={async (sessionId, cols, rows) => {
            try {
              await resizeTerminal(sessionId, cols, rows);
            } catch (error) {
              patchCodex({ error: String(error) });
            }
          }}
          onWorkDirChange={setCodexWorkDir}
        />
      </div>
    </div>
  );
}

interface PaneProps {
  agent: { id: TerminalAgent; label: string; description: string };
  paneKey: string;
  workspaceActive: boolean;
  surfaceVisible: boolean;
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
  paneKey,
  workspaceActive,
  surfaceVisible,
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
  const fitRef = useRef<FitAddon | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const [terminalReady, setTerminalReady] = useState(false);
  const [terminalError, setTerminalError] = useState("");

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

  const sessionDir = pane.session?.work_dir ?? "";
  const claudeDirMismatch =
    agent.id === "claude" &&
    pane.status === "running" &&
    Boolean(sessionDir) &&
    !samePath(sessionDir, workDir);

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
      {claudeDirMismatch && (
        <div className="terminal-pane__mismatch" role="status">
          当前 Claude 终端对话属于「{basenameOf(sessionDir)}」，与激活空间「{basenameOf(workDir)}」不同。停止后重新启动才会切到新空间。
        </div>
      )}
      <TerminalSurface
        agent={agent}
        paneKey={paneKey}
        workspaceActive={workspaceActive}
        surfaceVisible={surfaceVisible}
        pane={pane}
        onMount={(terminal) => {
          terminalRef.current = terminal;
          setTerminalReady(true);
          onMount(terminal);
        }}
        onUnmount={() => {
          terminalRef.current = null;
          fitRef.current = null;
          setTerminalReady(false);
          onUnmount();
        }}
        onFitAddon={(fit) => {
          fitRef.current = fit;
        }}
        onInput={(data) => onInput(agent.id, data)}
        onResize={onResize}
        onLoadError={setTerminalError}
      />
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

interface SurfaceProps {
  agent: { id: TerminalAgent; label: string; description: string };
  paneKey: string;
  workspaceActive: boolean;
  surfaceVisible: boolean;
  pane: PaneState;
  onMount: (terminal: Terminal) => void;
  onUnmount: () => void;
  onFitAddon?: (fit: FitAddon | null) => void;
  onInput: (data: string) => void;
  onResize: (sessionId: string, cols: number, rows: number) => void;
  onLoadError?: (message: string) => void;
}

function TerminalSurface({
  agent,
  workspaceActive,
  surfaceVisible,
  pane,
  onMount,
  onUnmount,
  onFitAddon,
  onInput,
  onResize,
  onLoadError,
}: SurfaceProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const sessionRef = useRef(pane.session);
  const layoutActiveRef = useRef(workspaceActive && surfaceVisible);
  const mountingRef = useRef(false);
  const mountedRef = useRef(false);
  const unmountedRef = useRef(false);
  const dataListenerRef = useRef<{ dispose: () => void } | null>(null);
  const cursorPinRef = useRef<{ dispose: () => void } | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const fitFrameRef = useRef<number | null>(null);
  const lastPtySizeRef = useRef("");
  const callbacksRef = useRef({ onMount, onUnmount, onInput, onResize, onFitAddon, onLoadError });

  useEffect(() => {
    sessionRef.current = pane.session;
    const terminal = terminalRef.current;
    if (pane.session && terminal) {
      lastPtySizeRef.current = `${pane.session.id}:${terminal.cols}x${terminal.rows}`;
    } else if (!pane.session) {
      lastPtySizeRef.current = "";
    }
  }, [pane.session]);

  useEffect(() => {
    layoutActiveRef.current = workspaceActive && surfaceVisible;
  }, [workspaceActive, surfaceVisible]);

  useEffect(() => {
    if (workspaceActive && surfaceVisible) {
      terminalRef.current?.focus();
    }
  }, [workspaceActive, surfaceVisible]);

  useEffect(() => {
    callbacksRef.current = { onMount, onUnmount, onInput, onResize, onFitAddon, onLoadError };
  }, [onInput, onMount, onResize, onUnmount, onFitAddon, onLoadError]);

  const scheduleFitAndResize = useCallback(() => {
    if (!layoutActiveRef.current || fitFrameRef.current !== null) return;
    fitFrameRef.current = -1;
    const run = () => {
      fitFrameRef.current = null;
      if (!layoutActiveRef.current) return;
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
    callbacksRef.current.onFitAddon?.(null);
    if (mountedRef.current) {
      mountedRef.current = false;
      callbacksRef.current.onUnmount();
    }
  }, []);

  // Mount xterm while the terminals workspace is open (all Claude tabs keep instances).
  useEffect(() => {
    if (!workspaceActive || !hostRef.current || terminalRef.current || mountingRef.current) return;
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
        callbacksRef.current.onFitAddon?.(fit);
        try {
          fit.fit();
        } catch {
          // Cell metrics can still be 0; scheduleFitAndResize retries.
        }
        callbacksRef.current.onMount(terminal);
        mountedRef.current = true;
        writeIdleBanner(terminal, agent);
        dataListenerRef.current = terminal.onData((data) => callbacksRef.current.onInput(data));
        terminal.focus();
        if (agent.id === "codex") {
          cursorPinRef.current = pinCursorSteady(terminal);
          terminal.write(CODEX_CURSOR_PIN);
        }
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
        if (!unmountedRef.current) {
          callbacksRef.current.onLoadError?.(`终端渲染器加载失败: ${String(error)}`);
        }
      })
      .finally(() => {
        mountingRef.current = false;
      });
  }, [workspaceActive, agent.description, agent.id, agent.label, scheduleFitAndResize]);

  useEffect(() => {
    if (!workspaceActive || !surfaceVisible || !terminalRef.current) return;
    scheduleFitAndResize();
  }, [workspaceActive, surfaceVisible, pane.session, scheduleFitAndResize]);

  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    // starting 清屏由 startPane 同步 reset，避免 effect 晚到把 PTY 欢迎输出清成黑屏
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

  return (
    <div ref={hostRef} className="terminal-surface" aria-label={`${agent.label} 终端输出`} />
  );
}
