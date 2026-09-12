import { useCallback, useEffect, useImperativeHandle, useRef, useState, type CSSProperties, type Ref } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { FitAddon } from "@xterm/addon-fit";
import type { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { resizeTerminal, startTerminal, stopTerminal, writeTerminal } from "../lib/terminalApi";
import { buildXtermOptions, CODEX_CURSOR_PIN, createOutputStabilizer, loadXtermRuntime, pinCursorSteady } from "../lib/xtermRuntime";
import { listenWhileMounted } from "../lib/listenWhileMounted";
import { useAgentCatalog } from "../lib/useAgentCatalog";
import type { TerminalAgent, TerminalSessionInfo, TerminalStatus } from "../types";
import { basenameOf, samePath } from "../lib/workspaces";
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

const AGENTS: { id: TerminalAgent; label: string; description: string }[] = [
  { id: "claude", label: "Claude CLI", description: "本机 Claude Code CLI" },
  { id: "codex", label: "Codex CLI", description: "本机 codex CLI" },
];

/**
 * 工作台两栏对称：左 = 被监督方（工人），右 = 监督方（审查者）。
 * 每栏上方一个 Agent 选择器，选谁显示谁的终端；
 * 每个 Agent 每栏最多一个 pane（paneKey = `side:agentId`），不会堆叠标签。
 */
type SideKey = "worker" | "reviewer";

interface SideState {
  /** 当前选中的 Agent（选择器值） */
  agentId: string;
  /** agentId → pane（保留各 Agent 的终端实例，切换选择不丢会话） */
  panes: Record<string, PaneState>;
}

const SIDE_LABEL: Record<SideKey, string> = {
  worker: "被监督方",
  reviewer: "监督方",
};

/** 侧选择持久化键（与监督表单共用，保持两处选择一致） */
const SIDE_AGENT_STORAGE: Record<SideKey, string> = {
  worker: "ha-worker-agent",
  reviewer: "ha-reviewer-agent",
};

function safeGetItem(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function safeSetItem(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // 存储不可用（隐私模式等）：选择仅本次会话有效
  }
}

function paneKeyOf(side: SideKey, agentId: string) {
  return `${side}:${agentId}`;
}

function parsePaneKey(paneKey: string): { side: SideKey; agentId: string } | null {
  const m = paneKey.match(/^(worker|reviewer):(.+)$/);
  return m ? { side: m[1] as SideKey, agentId: m[2] } : null;
}

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
  focusSession: (sessionId: string) => void;
  /** 把键盘焦点交给被监督方当前选中的 xterm（切到终端 tab 后收键） */
  focusWorkerPane: () => void;
  /** 找到指定 Agent 在该目录下的空闲已启动 pane，返回 session id（多 Agent 驱动用） */
  claimIdleAgentPane: (agentId: string, workDir: string) => string | null;
}

interface Props {
  active: boolean;
  onRunningChange?: (count: number) => void;
  /** 项目工作目录（工人侧的共享上下文，由 App 持有，监督闭环同源） */
  projectWorkDir?: string;
  onProjectWorkDirChange?: (dir: string) => void;
  ref?: Ref<TerminalWorkspaceHandle>;
}

export function TerminalWorkspace({ active, onRunningChange, projectWorkDir, onProjectWorkDirChange, ref }: Props) {
  const catalog = useAgentCatalog();
  /** 后端通知横幅（预信任失败等：不致命，但用户必须在启动终端前看见） */
  const [terminalNotice, setTerminalNotice] = useState("");

  const workerSelInit = safeGetItem(SIDE_AGENT_STORAGE.worker) || "claude";
  const reviewerSelInit = safeGetItem(SIDE_AGENT_STORAGE.reviewer) || "codex";
  const [workerSide, setWorkerSide] = useState<SideState>(() => ({
    agentId: workerSelInit,
    panes: { [workerSelInit]: initialPane() },
  }));
  const [reviewerSide, setReviewerSide] = useState<SideState>(() => ({
    agentId: reviewerSelInit,
    panes: { [reviewerSelInit]: initialPane() },
  }));
  const workerSideRef = useRef(workerSide);
  const reviewerSideRef = useRef(reviewerSide);

  const terminals = useRef(new Map<string, Terminal>());
  const inputQueuesRef = useRef(new Map<string, Promise<void>>());
  const stabilizersRef = useRef(new Map<string, ReturnType<typeof createOutputStabilizer>>());
  const noticeTimerRef = useRef<number | null>(null);
  const pendingOutputRef = useRef(new Map<string, { sessionId: string; data: string }>());
  /** sessionId → 尚未绑定到 pane 的输出（start_terminal 返回前 PTY 已可能吐字） */
  const orphanOutputRef = useRef(new Map<string, string>());
  /** sessionId → 早于 pane 绑定到达的退出事件（CLI 秒退/崩溃时 terminal-exit 先于 invoke 返回） */
  const orphanExitRef = useRef(new Map<string, { code?: number | null }>());
  const outputFrameRef = useRef<number | null>(null);
  const workerStartChainRef = useRef(Promise.resolve());
  const [reviewerWorkDir, setReviewerWorkDir] = useStoredString(
    "ha-workdir-reviewer",
    localStorage.getItem("ha-workdir-codex") ?? "",
  );
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

  const sideRef = useCallback(
    (side: SideKey) => (side === "worker" ? workerSideRef : reviewerSideRef),
    [],
  );

  const patchPane = useCallback((paneKey: string, patch: Partial<PaneState>) => {
    const parsed = parsePaneKey(paneKey);
    if (!parsed) return;
    const ref = parsed.side === "worker" ? workerSideRef : reviewerSideRef;
    const commit = parsed.side === "worker" ? setWorkerSide : setReviewerSide;
    const current = ref.current.panes[parsed.agentId] ?? initialPane();
    const nextSide: SideState = {
      ...ref.current,
      panes: { ...ref.current.panes, [parsed.agentId]: { ...current, ...patch } },
    };
    ref.current = nextSide;
    commit(nextSide);
  }, []);

  /**
   * 切换一侧选中的 Agent。persist=false 用于程序化激活（监督驱动/会话跳转）——
   * 不覆写用户的角色记忆，避免驱动链路反向改写任务表单已记的选择。
   */
  const selectAgent = useCallback(
    (side: SideKey, agentId: string, persist = true) => {
    const panes = { ...sideRef(side).current.panes };
    if (!panes[agentId]) panes[agentId] = initialPane();
    const nextSide: SideState = { agentId, panes };
    const ref = sideRef(side);
    const commit = side === "worker" ? setWorkerSide : setReviewerSide;
    ref.current = nextSide;
    commit(nextSide);
    if (persist) safeSetItem(SIDE_AGENT_STORAGE[side], agentId);
  }, [sideRef]);

  /** tab 标题/横幅用 meta：静态表优先，注册表条目兜底（新 Agent 未适配静态文案时） */
  const agentMetaFor = useCallback(
    (agentId: string): { id: TerminalAgent; label: string; description: string } => {
      const known = AGENTS.find((a) => a.id === agentId);
      if (known) return known;
      const entry = catalog.find((c) => c.id === agentId);
      if (entry) {
        const desc = entry.name.toLowerCase().includes("cli")
          ? `本机 ${entry.name}`
          : `本机 ${entry.name} CLI`;
        return { id: agentId, label: entry.name, description: desc };
      }
      return { id: agentId, label: agentId, description: "本机 CLI" };
    },
    [catalog],
  );

  /** codex 输出经过稳定器平滑（ANSI 光标重绘）；其余 agent 原样透传 */
  const stabilizerFor = useCallback((paneKey: string, agentId: string) => {
    if (agentId !== "codex") return null;
    let s = stabilizersRef.current.get(paneKey);
    if (!s) {
      s = createOutputStabilizer();
      stabilizersRef.current.set(paneKey, s);
    }
    return s;
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
      const parsed = parsePaneKey(paneKey);
      const pane = parsed
        ? (parsed.side === "worker" ? workerSideRef : reviewerSideRef).current.panes[parsed.agentId]
        : undefined;
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
      const stabilizer = parsed ? stabilizerFor(paneKey, parsed.agentId) : null;
      const data = stabilizer ? stabilizer.push(chunk.data) : chunk.data;
      if (data) term.write(data);
    }
    if (deferred && outputFrameRef.current === null) {
      outputFrameRef.current = -1;
      const frame = window.requestAnimationFrame(flushOutput);
      if (outputFrameRef.current === -1) outputFrameRef.current = frame;
    }
  }, [stabilizerFor]);

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
      const parsed = parsePaneKey(paneKey);
      const stabilizer = parsed ? stabilizerFor(paneKey, parsed.agentId) : null;
      const data = stabilizer ? stabilizer.push(buffered) : buffered;
      if (data) term.write(data);
      return;
    }
    enqueueOutput(paneKey, sessionId, buffered);
  }, [enqueueOutput, stabilizerFor]);

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
    Object.values(workerSide.panes).filter((p) => p.status === "running").length +
    Object.values(reviewerSide.panes).filter((p) => p.status === "running").length;

  useEffect(() => {
    onRunningChange?.(runningCount);
  }, [onRunningChange, runningCount]);

  const findPaneKeyBySession = useCallback((sessionId: string): string | null => {
    for (const [agentId, pane] of Object.entries(workerSideRef.current.panes)) {
      if (pane.session?.id === sessionId) return paneKeyOf("worker", agentId);
    }
    for (const [agentId, pane] of Object.entries(reviewerSideRef.current.panes)) {
      if (pane.session?.id === sessionId) return paneKeyOf("reviewer", agentId);
    }
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
          // 秒退/崩溃）。此时会话尚未写入 pane，直接丢弃会永久卡在"运行中"：
          // 先缓冲，等 startPane 拿到 session 后立即应用。
          orphanExitRef.current.set(sessionId, { code });
          return;
        }
        orphanExitRef.current.delete(sessionId);
        patchPane(paneKey, {
          status: "exited",
          session: null,
          error: exitErrorMessage(code),
        });
      },
    );
    const stopError = listenWhileMounted<{ sessionId?: string; message: string }>(
      "terminal-error",
      (event) => {
        if (event.payload.sessionId) {
          const paneKey = findPaneKeyBySession(event.payload.sessionId);
          if (!paneKey) return;
          patchPane(paneKey, { status: "error", error: event.payload.message });
          return;
        }
        for (const side of ["worker", "reviewer"] as SideKey[]) {
          const ref = sideRef(side);
          for (const agentId of Object.keys(ref.current.panes)) {
            patchPane(paneKeyOf(side, agentId), { status: "error", error: event.payload.message });
          }
        }
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
  }, [enqueueOutput, findPaneKeyBySession, patchPane]);

  const startPane = useCallback(
    async (
      paneKey: string,
      agent: TerminalAgent,
      cols: number,
      rows: number,
      opts?: StartOptions,
      options?: { skipBusyGate?: boolean },
    ): Promise<string | null> => {
      const parsed = parsePaneKey(paneKey);
      if (!parsed) return null;
      const pane = sideRef(parsed.side).current.panes[parsed.agentId] ?? initialPane();
      if (!options?.skipBusyGate && isBusyStatus(pane.status)) {
        patchPane(paneKey, { error: "终端已在运行：请先停止当前会话再续聊/启动" });
        return null;
      }
      const workDir = (
        opts?.workDir ?? (parsed.side === "worker" ? projectWorkDir ?? "" : reviewerWorkDir)
      ).trim();
      if (!workDir) {
        patchPane(paneKey, { status: "error" as const, error: "请输入工作目录" });
        return null;
      }
      patchPane(paneKey, { status: "starting", error: "" });
      if (agent === "codex") stabilizerFor(paneKey, agent)?.reset();
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
          patchPane(paneKey, {
            status: "exited",
            session: null,
            error: exitErrorMessage(orphanExit.code),
          });
          return null;
        }
        patchPane(paneKey, { session, status: "running", error: "" });
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
        patchPane(paneKey, { status: "error" as const, error: String(error) });
        return null;
      }
    },
    [claimOrphanOutput, patchPane, projectWorkDir, reviewerWorkDir, stabilizerFor],
  );

  useImperativeHandle(
    ref,
    () => ({
      startWith: (agent, opts) => {
        // 续聊归属侧：该 agent 已在某一侧有 pane 就地打开；否则按默认角色
        // 归属（claude → 工人侧，codex → 监督侧，其余工人候选 → 工人侧）
        const existingSide = (["worker", "reviewer"] as SideKey[]).find(
          (side) => sideRef(side).current.panes[agent] !== undefined,
        );
        const side: SideKey = existingSide ?? (agent === "codex" ? "reviewer" : "worker");
        selectAgent(side, agent);
        if (opts?.workDir !== undefined) {
          if (side === "worker") onProjectWorkDirChange?.(opts.workDir);
          else setReviewerWorkDir(opts.workDir);
        }
        const paneKey = paneKeyOf(side, agent);
        const terminal = terminals.current.get(paneKey);
        void startPane(paneKey, agent, terminal?.cols ?? 120, terminal?.rows ?? 30, opts);
      },
      startClaudeForTask: (workDir) => {
        const run = async () => {
          if (workDir !== undefined) onProjectWorkDirChange?.(workDir);
          // 程序化激活：不改写用户在任务表单记下的工人选择
          selectAgent("worker", "claude", false);
          // 驱动任务必须换新 PTY：旧会话（同目录冻在信任菜单的、或切了工作区
          // 遗留在别处的）一律先停——单 pane 模型下直接换绑 session 会让旧 PTY
          // 失去 UI 归属，其输出/退出事件落入 orphan 缓冲永不清理（泄漏）
          const staleSession = workerSideRef.current.panes.claude?.session?.id;
          if (staleSession) {
            patchPane(paneKeyOf("worker", "claude"), { status: "stopping", error: "" });
            try {
              await stopTerminal(staleSession);
            } catch {
              // 旧进程杀不掉也不阻断新启动；新 pane 才是可交互的那一个。
            }
            patchPane(paneKeyOf("worker", "claude"), { status: "exited", session: null, error: "" });
          }
          const paneKey = paneKeyOf("worker", "claude");
          const terminal = terminals.current.get(paneKey);
          const sessionId = await startPane(
            paneKey,
            "claude",
            terminal?.cols ?? 120,
            terminal?.rows ?? 30,
            { workDir },
            { skipBusyGate: true },
          );
          if (!sessionId) throw new Error("无法启动 Claude 终端");
          return sessionId;
        };
        const queued = workerStartChainRef.current.then(run, run);
        workerStartChainRef.current = queued.then(
          () => undefined,
          () => undefined,
        );
        return queued;
      },
      focusSession: (sessionId) => {
        for (const side of ["worker", "reviewer"] as SideKey[]) {
          const panes = sideRef(side).current.panes;
          for (const [agentId, pane] of Object.entries(panes)) {
            if (pane.session?.id === sessionId) {
              selectAgent(side, agentId, false);
              terminals.current.get(paneKeyOf(side, agentId))?.focus();
              return;
            }
          }
        }
      },
      focusWorkerPane: () => {
        terminals.current.get(paneKeyOf("worker", workerSideRef.current.agentId))?.focus();
      },
      claimIdleAgentPane: (agentId, workDir) => {
        const pane = workerSideRef.current.panes[agentId];
        if (
          pane?.session?.id &&
          !isBusyStatus(pane.status) &&
          samePath(pane.session.work_dir ?? "", workDir.trim())
        ) {
          selectAgent("worker", agentId, false);
          return pane.session.id;
        }
        return null;
      },
    }),
    [onProjectWorkDirChange, patchPane, projectWorkDir, selectAgent, setReviewerWorkDir, startPane],
  );

  const handleStop = useCallback(
    async (paneKey: string) => {
      const parsed = parsePaneKey(paneKey);
      if (!parsed) return;
      const pane = sideRef(parsed.side).current.panes[parsed.agentId];
      const session = pane?.session;
      if (!session) return;
      patchPane(paneKey, { status: "stopping", error: "" });
      try {
        await stopTerminal(session.id);
      } catch (error) {
        const still = sideRef(parsed.side).current.panes[parsed.agentId]?.session?.id === session.id;
        if (still) {
          patchPane(paneKey, { status: "running" as const, error: String(error) });
        }
      }
    },
    [patchPane],
  );

  const handleInput = useCallback(
    (paneKey: string, data: string) => {
      const parsed = parsePaneKey(paneKey);
      if (!parsed) return;
      const session = sideRef(parsed.side).current.panes[parsed.agentId]?.session;
      if (!session) return;
      const sessionId = session.id;
      const write = async () => {
        const current = sideRef(parsed.side).current.panes[parsed.agentId]?.session?.id;
        if (current !== sessionId) return;
        try {
          await writeTerminal(sessionId, data);
        } catch (error) {
          patchPane(paneKey, { status: "error" as const, error: String(error) });
        }
      };
      const prev = inputQueuesRef.current.get(paneKey) ?? Promise.resolve();
      inputQueuesRef.current.set(
        paneKey,
        prev.catch(() => undefined).then(write),
      );
    },
    [patchPane],
  );

  /** 一侧的渲染：选择器 + 各 agent 的 pane（选中的可见，其余保持挂载保实例） */
  const renderSide = (side: SideKey) => {
    const state = side === "worker" ? workerSide : reviewerSide;
    const setDir = side === "worker"
      ? (dir: string) => onProjectWorkDirChange?.(dir)
      : setReviewerWorkDir;
    const sideWorkDir = side === "worker" ? projectWorkDir ?? "" : reviewerWorkDir;
    // 工人侧限 can_work（要开交互终端）；监督侧放宽到 can_review 联合——
    // 未来纯审查型 agent 也能出现在右侧
    const options = catalog.filter((c) =>
      side === "worker" ? c.can_work : c.can_work || c.can_review,
    );
    const selectOptions: { id: string; name: string; installed: boolean }[] = options.length
      ? options
      : [{ id: side === "worker" ? "claude" : "codex", name: side === "worker" ? "Claude Code" : "Codex CLI", installed: true }];
    return (
      <div className="terminal-side" data-side={side}>
        <div className="terminal-side__head">
          <span className={`terminal-side__role terminal-side__role--${side}`}>
            {side === "worker" ? <Icon name="spark" size={13} /> : <Icon name="shield" size={13} />}
            {SIDE_LABEL[side]}
          </span>
          <select
            className="terminal-side__select"
            value={state.agentId}
            onChange={(e) => selectAgent(side, e.currentTarget.value)}
            aria-label={`${SIDE_LABEL[side]} Agent`}
            title={`切换${SIDE_LABEL[side]}的 CLI Agent（每个 Agent 保留独立终端）`}
          >
            {selectOptions.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
                {c.installed ? "" : "（未检测到安装）"}
              </option>
            ))}
          </select>
        </div>
        {Object.entries(state.panes).map(([agentId, pane]) => {
          const paneKey = paneKeyOf(side, agentId);
          const visible = agentId === state.agentId;
          return (
            <div
              key={agentId}
              className={`terminal-side-host ${visible ? "is-active" : ""}`}
              hidden={!visible}
              style={{ display: visible ? "flex" : "none" }}
            >
              <TerminalPane
                agent={agentMetaFor(agentId)}
                paneKey={paneKey}
                workspaceActive={active && visible}
                surfaceVisible={visible}
                pane={pane}
                workDir={sideWorkDir}
                onMount={(terminal) => mountTerminal(paneKey, terminal)}
                onUnmount={() => unmountTerminal(paneKey)}
                onStart={(_agent, cols, rows) => {
                  void startPane(paneKey, agentId, cols, rows);
                }}
                onStop={() => {
                  void handleStop(paneKey);
                }}
                onInput={(_agent, data) => handleInput(paneKey, data)}
                onResize={async (sessionId, cols, rows) => {
                  try {
                    await resizeTerminal(sessionId, cols, rows);
                  } catch (error) {
                    patchPane(paneKey, { error: String(error) });
                  }
                }}
                onWorkDirChange={setDir}
              />
            </div>
          );
        })}
      </div>
    );
  };

  return (
    <div className={`terminal-workspace ${active ? "is-active" : ""}`}>
      <div className="terminal-intro">
        <div>
          <span className="eyebrow">LOCAL CLI WORKBENCH</span>
          <h2>本地 CLI 工作台</h2>
          <p>左侧是被监督方（干活），右侧是监督方（审查）。两侧各选一个 CLI Agent，选中即显示其终端；切换工作区不会结束进程。</p>
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
        {renderSide("worker")}
        <SplitHandle
          orientation={stacked ? "horizontal" : "vertical"}
          label="调整被监督方与监督方终端区域"
          value={ratio}
          min={minRatio}
          max={maxRatio}
          onChange={setRatio}
          pixelsPerUnit={Math.max(1, axisSize - SPLITTER_SIZE) / 100}
          step={5}
          className="terminal-split"
          valueText={`被监督方终端区域占比 ${Math.round(ratio)}%`}
        />
        {renderSide("reviewer")}
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

  // Mount xterm while the terminals workspace is open (all panes keep instances).
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
