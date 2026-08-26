import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { SessionList, resumeStart } from "./components/SessionList";
import { TranscriptView } from "./components/TranscriptView";
import { SearchBox } from "./components/SearchBox";
import { SupervisePanel } from "./components/SupervisePanel";
import { ReviewBoard } from "./components/ReviewBoard";
import { TaskList } from "./components/TaskList";
import { WorkspaceSidebar } from "./components/WorkspaceSidebar";
import { MCPStatusPanel } from "./components/MCPStatusPanel";
import { open, confirm } from "@tauri-apps/plugin-dialog";
import { Icon, IconButton, type IconName } from "./components/Icon";
import { StatusBar } from "./components/StatusBar";
import { TerminalWorkspace, type TerminalWorkspaceHandle } from "./components/TerminalWorkspace";
import { SplitHandle } from "./components/SplitHandle";
import harnessMark from "./assets/harness-mark.svg";
import { fetchSessions, fetchTranscript, searchSessions, fetchTasks, cancelSupervise, retrySuperviseReview } from "./lib/api";
import { formatFull } from "./lib/formatTime";
import { useElementSize, useMediaQuery, useStoredNumber } from "./lib/layoutPreferences";
import { listenWhileMounted } from "./lib/listenWhileMounted";
import { addWorkspace, initWorkspaces, saveWorkspaces, tasksForWorkspace } from "./lib/workspaces";
import type { SessionInfo, TaskInfo, TranscriptEntry } from "./types";
import pkg from "../package.json";
import "./App.css";

type Tab = "sessions" | "supervise" | "mcp" | "terminals";
type Theme = "dark" | "light";
type AgentFilter = "all" | "claude" | "codex";
type McpHealth = "checking" | "healthy" | "degraded";

const NAV_ITEMS: { id: Tab; label: string; icon: IconName; key: string; detail: string }[] = [
  { id: "sessions", label: "会话浏览", icon: "message", key: "1", detail: "Claude / Codex transcript" },
  { id: "terminals", label: "终端工作台", icon: "terminal", key: "2", detail: "Local CLI sessions" },
  { id: "supervise", label: "监督闭环", icon: "shield", key: "3", detail: "Run → review → iterate" },
  { id: "mcp", label: "MCP 状态", icon: "plug", key: "4", detail: "Connection health" },
];

const SESSION_SIDEBAR_MIN = 260;
const SESSION_SIDEBAR_MAX = 440;
const SESSION_READER_TARGET_MIN = 420;
const SPLITTER_HIT_AREA = 12;
const NAV_WIDTH_MIN = 208;
const NAV_WIDTH_MAX = 320;
const SUPERVISE_WIDTH_MIN = 320;
const SUPERVISE_WIDTH_MAX = 620;
const SUPERVISE_HEIGHT_MIN = 240;
const SUPERVISE_HEIGHT_MAX = 520;
const TRANSCRIPT_PAGE = 200;           // 会话正文每页条数（与 server.js MAX_LINES 对齐）

function clampWidth(width: number, min: number, max: number) {
  return Math.min(Math.max(width, min), Math.max(min, max));
}

function readTheme(): Theme {
  try {
    const saved = localStorage.getItem("ha-theme");
    return saved === "light" ? "light" : "dark";
  } catch {
    return "dark";
  }
}

function Empty({ children }: { children: React.ReactNode }) {
  return (
    <div className="empty-state">
      <div className="empty-state__icon"><Icon name="file" size={20} /></div>
      <div className="empty-state__copy">{children}</div>
    </div>
  );
}

function SkeletonList() {
  return (
    <div className="skeleton" aria-label="正在加载会话">
      {Array.from({ length: 8 }).map((_, i) => (
        <div className="skeleton-item" key={i}>
          <div className="skeleton-line" />
          <div className="skeleton-line short" />
        </div>
      ))}
    </div>
  );
}

function App() {
  const [tab, setTab] = useState<Tab>("sessions");
  const [theme, setTheme] = useState<Theme>(readTheme);
  const [navCollapsed, setNavCollapsed] = useState(false);
  const compactNav = useMediaQuery("(max-width: 1100px)");
  const compactSupervise = useMediaQuery("(max-width: 820px)");

  // ---- 会话浏览状态（常驻 App，切 tab 不丢失）----
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selected, setSelected] = useState<SessionInfo | null>(null);
  const [transcript, setTranscript] = useState<TranscriptEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadingTrans, setLoadingTrans] = useState(false);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const [transcriptHasMore, setTranscriptHasMore] = useState(false);
  const [error, setError] = useState("");
  const [searchKeyword, setSearchKeyword] = useState("");
  const [sessionLimit, setSessionLimit] = useState(50);
  const [agentFilter, setAgentFilter] = useState<AgentFilter>("all");
  const [mcpHealth, setMcpHealth] = useState<McpHealth>("checking");
  const transcriptCache = useRef(new Map<string, { entries: TranscriptEntry[]; hasMore: boolean }>());
  const activeFileRef = useRef<string | null>(null); // 翻页期间切走会话时丢弃过期结果

  // ---- 监督闭环状态（常驻 App）----
  // 工作空间一级实体（Codex Project 精神）：path 为身份、position 保序；
  // 项目工作目录 = 激活空间的 path（Claude 终端 pane 与监督表单共享）。
  const [workspaces, setWorkspaces] = useState(() => initWorkspaces());
  const activeWorkspace = workspaces.list.find((w) => w.id === workspaces.activeId) ?? workspaces.list[0] ?? null;
  const projectWorkDir = activeWorkspace?.path ?? "";
  const [terminalRunning, setTerminalRunning] = useState(0);
  const [tasks, setTasks] = useState<TaskInfo[]>([]);
  const activeTasks = tasksForWorkspace(tasks, projectWorkDir);
  const [focusedTaskId, setFocusedTaskId] = useState<string | null>(null);
  const focusedTaskIdEffective = useMemo(() => {
    if (focusedTaskId && activeTasks.some((t) => t.id === focusedTaskId)) return focusedTaskId;
    const running = activeTasks.filter((t) => t.status === "running");
    const pool = running.length > 0 ? running : activeTasks;
    if (pool.length === 0) return null;
    return [...pool].sort((a, b) => b.started_at_ms - a.started_at_ms)[0].id;
  }, [activeTasks, focusedTaskId]);
  const superviseRunning = tasks.filter((t) => t.status === "running").length;

  // 目录上报（终端 pane 输入/续聊）：与侧栏「+」同语义——命中既有空间→激活；
  // 不命中→新建空间并激活；绝不原地改写既有空间路径（杜绝覆盖事故）
  const handleWorkDirChange = useCallback((rawDir: string) => {
    const next = addWorkspace(workspaces.list, workspaces.activeId, rawDir);
    if (next.changed) {
      setWorkspaces(next);
    }
  }, [workspaces]);

  // 持久化 workspaces 列表 + 激活 id；旧单目录键已由 initWorkspaces 一次性迁移并清除
  useEffect(() => {
    saveWorkspaces(workspaces.list, workspaces.activeId);
  }, [workspaces]);

  /** 切换激活空间 */
  const handleSelectWorkspace = useCallback((id: string) => {
    setWorkspaces((prev) => ({ ...prev, activeId: id }));
  }, []);

  /** 添加工作空间：打开系统目录选择器，创建新空间并激活 */
  const handleAddWorkspace = useCallback(async () => {
    try {
      const dir = await open({
        directory: true,
        multiple: false,
        title: "选择项目工作目录作为工作空间",
      });
      if (typeof dir !== "string" || !dir.trim()) return;
      // 信任确认（Codex 精神：说明该目录将被注入任务/写产物）
      const trusted = await confirm(
        `目录 "${dir}" 将被用于注入监督任务和执行代码审查，` +
        "并会在其中写入审查产物（.supervise/）。确认添加此项目目录作为工作空间吗？",
        { title: "添加工作空间", kind: "info" },
      );
      if (!trusted) return;
      // 追加语义（不是改目录）：命中既有空间→激活；否则新建，绝不覆盖既有空间
      const next = addWorkspace(workspaces.list, workspaces.activeId, dir.trim());
      if (next.changed) {
        setWorkspaces(next);
      }
    } catch {
      // 用户取消选择或无权限
    }
  }, [workspaces]);

  /** 移除工作空间 */
  const handleRemoveWorkspace = useCallback((id: string) => {
    setWorkspaces((prev) => {
      const filtered = prev.list.filter((w) => w.id !== id);
      const newActive = prev.activeId === id ? (filtered[0]?.id ?? null) : prev.activeId;
      return { list: filtered, activeId: newActive };
    });
  }, []);

  /** 重命名工作空间 */
  const handleRenameWorkspace = useCallback((id: string, name: string) => {
    setWorkspaces((prev) => ({
      ...prev,
      list: prev.list.map((w) => (w.id === id ? { ...w, name } : w)),
    }));
  }, []);

  /** 加载任务列表（从后端任务注册表） */
  const loadTasks = useCallback(async () => {
    try {
      setTasks(await fetchTasks());
    } catch {
      // 列表加载失败不阻断 UI
    }
  }, []);

  // 切入监督 tab 时刷新任务列表
  useEffect(() => {
    if (tab === "supervise") void loadTasks();
  }, [tab, loadTasks]);

  // 监听 supervise-done 事件自动刷新任务列表（运行中→终态更新）
  useEffect(() => {
    const stop = listenWhileMounted("supervise-done", () => {
      void loadTasks();
    });
    return stop;
  }, [loadTasks]);

  /** 取消运行中的监督任务 */
  const handleCancelTask = useCallback(async (taskId: string) => {
    try {
      await cancelSupervise(taskId);
    } catch {
      // 取消失败静默
    }
  }, []);

  /** 重试审查：修好 Codex 后由用户主动接续 */
  const handleRetryReview = useCallback(async (taskId: string) => {
    setError("");
    try {
      await retrySuperviseReview(taskId);
      void loadTasks();
    } catch (e) {
      setError(String(e));
    }
  }, [loadTasks]);

  /** 终端驱动监督启动后切到终端 tab */
  const handleDriveStarted = useCallback(() => {
    setTab("terminals");
  }, []);

  const terminalRef = useRef<TerminalWorkspaceHandle>(null);

  /** 会话列表「在终端中续聊」：切到终端工作台，以对应 CLI 的 resume 参数启动；
   *  会话带原始 cwd（Claude JSONL 记录）则还原该会话的工作目录 */
  const handleResumeInTerminal = useCallback((s: SessionInfo) => {
    const start = resumeStart(s);
    setTab("terminals");
    terminalRef.current?.startWith(start.agent, { args: start.args, workDir: start.workDir });
  }, []);

  // ---- 可调面板宽度（可拖动分割线控制内容密度）----
  const [navWidth, setNavWidth] = useStoredNumber("ha-layout-nav-width", 248);
  const [sidebarWidth, setSidebarWidth] = useStoredNumber("ha-layout-session-sidebar-width", 320);
  const [panelWidth, setPanelWidth] = useStoredNumber("ha-layout-supervise-width", 420);
  const [panelHeight, setPanelHeight] = useStoredNumber("ha-layout-supervise-height", 320);
  const sessionLayoutRef = useRef<HTMLDivElement>(null);
  const superviseLayoutRef = useRef<HTMLDivElement>(null);
  const superviseSize = useElementSize(superviseLayoutRef);

  const getSessionSidebarMax = useCallback(() => {
    const layoutWidth = sessionLayoutRef.current?.clientWidth;
    if (!layoutWidth) return SESSION_SIDEBAR_MAX;
    return Math.max(
      SESSION_SIDEBAR_MIN,
      Math.min(SESSION_SIDEBAR_MAX, layoutWidth - SESSION_READER_TARGET_MIN - SPLITTER_HIT_AREA),
    );
  }, []);

  const getSupervisePanelMax = useCallback(() => {
    if (compactSupervise) {
      if (!superviseSize.height) return SUPERVISE_HEIGHT_MAX;
      return Math.max(
        SUPERVISE_HEIGHT_MIN,
        Math.min(SUPERVISE_HEIGHT_MAX, superviseSize.height - 240 - SPLITTER_HIT_AREA),
      );
    }
    if (!superviseSize.width) return SUPERVISE_WIDTH_MAX;
    return Math.max(
      SUPERVISE_WIDTH_MIN,
      Math.min(SUPERVISE_WIDTH_MAX, superviseSize.width - 360 - SPLITTER_HIT_AREA),
    );
  }, [compactSupervise, superviseSize.height, superviseSize.width]);

  useLayoutEffect(() => {
    const constrainSidebar = () => {
      setSidebarWidth((current) => clampWidth(current, SESSION_SIDEBAR_MIN, getSessionSidebarMax()));
    };

    constrainSidebar();
    if (typeof ResizeObserver === "undefined" || !sessionLayoutRef.current) {
      window.addEventListener("resize", constrainSidebar);
      return () => window.removeEventListener("resize", constrainSidebar);
    }

    const observer = new ResizeObserver(constrainSidebar);
    observer.observe(sessionLayoutRef.current);
    return () => observer.disconnect();
  }, [getSessionSidebarMax]);

  useEffect(() => {
    if (compactSupervise) {
      setPanelHeight((current) => clampWidth(current, SUPERVISE_HEIGHT_MIN, getSupervisePanelMax()));
    } else {
      setPanelWidth((current) => clampWidth(current, SUPERVISE_WIDTH_MIN, getSupervisePanelMax()));
    }
  }, [compactSupervise, getSupervisePanelMax, setPanelHeight, setPanelWidth]);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem("ha-theme", theme);
    } catch {
      // Theme persistence is a convenience; the UI still works when storage is blocked.
    }
    void getCurrentWindow().setTheme(theme).catch(() => {
      // Browser-only previews do not expose a native title bar.
    });
  }, [theme]);

  // Ctrl/Cmd + 1–4 switches workspace. Input controls own their shortcuts.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.ctrlKey || event.metaKey) || event.shiftKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (target instanceof HTMLElement && target.matches("input, textarea, select, [contenteditable='true']")) return;
      const item = NAV_ITEMS.find((nav) => nav.key === event.key);
      if (!item) return;
      event.preventDefault();
      setTab(item.id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const loadSessions = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setSessions(await fetchSessions(undefined, sessionLimit));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, [sessionLimit]);

  const handleSearch = useCallback(async (keyword: string) => {
    setSearchKeyword(keyword);
    setLoading(true);
    setError("");
    try {
      setSessions(await searchSessions(keyword));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (tab !== "sessions") return;
    if (searchKeyword) {
      void handleSearch(searchKeyword);
    } else {
      void loadSessions();
    }
    // Search keyword intentionally triggers its own request above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab, loadSessions]);

  const handleClear = useCallback(() => {
    setSearchKeyword("");
    void loadSessions();
  }, [loadSessions]);

  const handleRefresh = useCallback(() => {
    if (searchKeyword) {
      void handleSearch(searchKeyword);
    } else {
      void loadSessions();
    }
  }, [handleSearch, loadSessions, searchKeyword]);

  const selectSession = useCallback(async (session: SessionInfo) => {
    setSelected(session);
    activeFileRef.current = session.file;
    setError("");
    setTranscriptHasMore(false);
    const cached = transcriptCache.current.get(session.file);
    if (cached) {
      setTranscript(cached.entries);
      setTranscriptHasMore(cached.hasMore);
      return;
    }
    setLoadingTrans(true);
    try {
      const entries = await fetchTranscript(session.file, TRANSCRIPT_PAGE);
      const hasMore = entries.length === TRANSCRIPT_PAGE;
      setTranscript(entries);
      setTranscriptHasMore(hasMore);
      transcriptCache.current.set(session.file, { entries, hasMore });
      if (transcriptCache.current.size > 50) {
        const first = transcriptCache.current.keys().next().value;
        if (first) transcriptCache.current.delete(first);
      }
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoadingTrans(false);
    }
  }, []);

  /** 审看看板跳转：按文件路径直接打开 transcript（监督会话是活文件，绕过缓存） */
  const openTranscriptByFile = useCallback((file: string) => {
    transcriptCache.current.delete(file);
    void selectSession({
      agent: "claude",
      agentLabel: "Claude Code",
      file,
      title: "",
      updated: "",
    });
    setTab("sessions");
  }, [selectSession]);

  const loadEarlier = useCallback(async () => {
    if (!selected || !transcriptHasMore || loadingEarlier) return;
    const file = selected.file;
    setLoadingEarlier(true);
    setError("");
    try {
      const page = await fetchTranscript(file, TRANSCRIPT_PAGE, transcript.length);
      if (activeFileRef.current !== file) return; // 翻页期间切走了会话，丢弃过期结果
      // 会话文件可能仍在增长：以 at|text 为键去重，避免翻页时重复插入同一条
      const seen = new Set(transcript.map((e) => `${e.at ?? ""}|${e.text}`));
      const fresh = page.filter((e) => !seen.has(`${e.at ?? ""}|${e.text}`));
      if (page.length === 0 || fresh.length === 0) {
        setTranscriptHasMore(false);
        return;
      }
      const merged = [...fresh, ...transcript];
      const hasMore = page.length === TRANSCRIPT_PAGE;
      setTranscript(merged);
      setTranscriptHasMore(hasMore);
      transcriptCache.current.set(file, { entries: merged, hasMore });
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoadingEarlier(false);
    }
  }, [selected, transcript, transcriptHasMore, loadingEarlier]);

  const handleMcpHealthChange = useCallback((health: McpHealth) => {
    setMcpHealth(health);
  }, []);

  const filteredSessions = useMemo(
    () => agentFilter === "all" ? sessions : sessions.filter((session) => session.agent === agentFilter),
    [agentFilter, sessions],
  );
  const claudeCount = sessions.filter((session) => session.agent === "claude").length;
  const codexCount = sessions.filter((session) => session.agent === "codex").length;
  const currentNav = NAV_ITEMS.find((item) => item.id === tab) ?? NAV_ITEMS[0];
  const mcpLabel = mcpHealth === "healthy" ? "MCP 健康" : mcpHealth === "degraded" ? "MCP 需要处理" : "MCP 检查中";

  return (
    <main className={`app-shell ${navCollapsed ? "nav-is-collapsed" : ""}`}>
      <nav
        className="workspace-nav"
        aria-label="工作区导航"
        style={!navCollapsed && !compactNav ? { width: navWidth, flexBasis: navWidth } : undefined}
      >
        <div className="workspace-nav__brand">
          <img className="brand-mark" src={harnessMark} alt="" />
          {!navCollapsed && (
            <div className="brand-copy">
              <strong>Harness Agent</strong>
              <span>developer workbench</span>
            </div>
          )}
        </div>
        <div className="workspace-nav__section-label">WORKSPACES</div>
        <div className="workspace-nav__items">
          {NAV_ITEMS.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`workspace-nav__item ${tab === item.id ? "is-active" : ""}`}
              onClick={() => setTab(item.id)}
              aria-label={item.label}
              title={`${item.label} · Ctrl+${item.key}`}
            >
              <span className="workspace-nav__icon"><Icon name={item.icon} size={17} /></span>
              {!navCollapsed && (
                <span className="workspace-nav__label">
                  <strong>{item.label}</strong>
                  <small>{item.detail}</small>
                </span>
              )}
              <kbd>{item.key}</kbd>
            </button>
          ))}
        </div>
        <div className="workspace-nav__spacer" />
        <div className="workspace-nav__status">
          <div className="nav-status-head">
            <span className={`status-indicator status-indicator--${mcpHealth}`} />
            {!navCollapsed && <span>{mcpLabel}</span>}
          </div>
          {!navCollapsed && (
            <div className="nav-agent-counts">
              <span><i className="agent-dot agent-dot--claude" /> Claude {claudeCount}</span>
              <span><i className="agent-dot agent-dot--codex" /> Codex {codexCount}</span>
            </div>
          )}
        </div>
        <div className="workspace-nav__footer">
          <IconButton
            label={navCollapsed ? "展开导航" : "收起导航"}
            onClick={() => setNavCollapsed((collapsed) => !collapsed)}
          >
            <Icon name="chevron-right" size={15} className={navCollapsed ? "" : "rotate-180"} />
          </IconButton>
          <IconButton
            label={theme === "dark" ? "切换浅色主题" : "切换深色主题"}
            onClick={() => setTheme((current) => current === "dark" ? "light" : "dark")}
          >
            <Icon name={theme === "dark" ? "sun" : "moon"} size={15} />
          </IconButton>
          {!navCollapsed && <span className="nav-version">v{pkg.version} · local</span>}
        </div>
      </nav>

      {!navCollapsed && !compactNav && (
        <SplitHandle
          orientation="vertical"
          label="调整工作区导航宽度"
          value={navWidth}
          min={NAV_WIDTH_MIN}
          max={NAV_WIDTH_MAX}
          onChange={setNavWidth}
          className="workspace-shell-split"
          valueText={`导航宽度 ${Math.round(navWidth)} 像素`}
        />
      )}

      <div className="workspace-main">
        <header className="topbar">
          <div className="topbar__title">
            <span className="eyebrow">HARNESS / {currentNav.label.toUpperCase()}</span>
            <h1>{currentNav.label}</h1>
          </div>
          {tab === "sessions" && (
            <div className="topbar__actions">
              <label className="select-control select-control--compact">
                <span className="sr-only">Agent 来源</span>
                <select value={agentFilter} onChange={(event) => setAgentFilter(event.currentTarget.value as AgentFilter)}>
                  <option value="all">全部来源</option>
                  <option value="claude">Claude 来源</option>
                  <option value="codex">Codex 来源</option>
                </select>
                <Icon name="chevron-down" size={13} />
              </label>
              <label className="select-control select-control--compact">
                <span className="sr-only">会话数量</span>
                <select
                  value={sessionLimit}
                  onChange={(event) => setSessionLimit(Number(event.currentTarget.value))}
                  title="每类 Agent 显示的会话数"
                >
                  <option value={20}>20 条</option>
                  <option value={50}>50 条</option>
                  <option value={100}>100 条</option>
                </select>
                <Icon name="chevron-down" size={13} />
              </label>
              <IconButton label="刷新会话列表" onClick={handleRefresh} disabled={loading}>
                <Icon name="refresh" size={16} className={loading ? "spin" : ""} />
              </IconButton>
              <SearchBox onSearch={handleSearch} onClear={handleClear} />
            </div>
          )}
          {tab === "terminals" && (
            <div className="topbar__context"><span className="live-dot" /> Local CLI · 按需启动</div>
          )}
          {tab === "supervise" && (
            <div className="topbar__context"><Icon name="shield" size={14} /> 任务状态由监督引擎实时推送</div>
          )}
          {tab === "mcp" && (
            <div className="topbar__context"><Icon name="link" size={14} /> agent-sessions bridge</div>
          )}
        </header>

        {error && (
          <div className="feedback-banner feedback-banner--error" role="alert">
            <Icon name="alert" size={16} />
            <span>{error}</span>
            <button type="button" onClick={() => setError("")} aria-label="关闭错误提示"><Icon name="close" size={15} /></button>
          </div>
        )}

        <section className={`view ${tab === "sessions" ? "active" : ""}`} aria-hidden={tab !== "sessions"}>
          <div className="session-layout" ref={sessionLayoutRef}>
            <aside className="session-sidebar" style={{ width: sidebarWidth }}>
              <div className="section-heading">
                <div>
                  <span className="eyebrow">RECENT ACTIVITY</span>
                  <h2>会话</h2>
                </div>
                <span className="count-pill">{filteredSessions.length}</span>
              </div>
              {searchKeyword && (
                <div className="search-banner">
                  <span><Icon name="search" size={13} /> 搜索 “{searchKeyword}” · {filteredSessions.length} 条</span>
                  <button type="button" onClick={handleClear} aria-label="清除搜索"><Icon name="close" size={13} /> 清除</button>
                </div>
              )}
              {loading && sessions.length === 0 ? (
                <SkeletonList />
              ) : (
                <SessionList
                  sessions={filteredSessions}
                  selectedFile={selected?.file ?? null}
                  onSelect={selectSession}
                  searching={Boolean(searchKeyword)}
                  onEscapeSearch={searchKeyword ? handleClear : undefined}
                  onResumeInTerminal={handleResumeInTerminal}
                />
              )}
            </aside>
            <SplitHandle
              orientation="vertical"
              label="调整会话列表宽度"
              value={sidebarWidth}
              min={SESSION_SIDEBAR_MIN}
              max={getSessionSidebarMax}
              onChange={setSidebarWidth}
              className="session-reader-split"
              valueText={`会话列表宽度 ${Math.round(sidebarWidth)} 像素`}
            />
            <section className="transcript-panel">
              {selected ? (
                <>
                  <header className="transcript-head">
                    <div className="transcript-head__identity">
                      <span className={`agent-avatar agent-avatar--${selected.agent}`}>
                        <Icon name={selected.agent === "claude" ? "spark" : "code"} size={16} />
                      </span>
                      <div>
                        <h2>{selected.title?.trim() || selected.agentLabel}</h2>
                        <span>{formatFull(selected.updated)}</span>
                      </div>
                    </div>
                    <div className="transcript-head__actions">
                      <span className="read-only-pill"><Icon name="file" size={12} /> JSONL transcript</span>
                      <span className="transcript-head__hint">{loadingTrans ? "同步中" : `${transcript.length} entries`}</span>
                    </div>
                  </header>
                  <div className="file-path-row" title={selected.file}>
                    <Icon name="folder" size={14} />
                    <code>{selected.file}</code>
                  </div>
                  {transcriptHasMore && (
                    <button type="button" className="load-earlier" onClick={loadEarlier} disabled={loadingEarlier}>
                      {loadingEarlier ? "正在加载更早…" : "加载更早的消息"}
                    </button>
                  )}
                  {loadingTrans && <div className="loading-bar" aria-label="正在加载正文"><span /></div>}
                  <TranscriptView entries={transcript} />
                </>
              ) : (
                <Empty>
                  <strong>从左侧选择会话</strong>
                  <span>查看 Claude 或 Codex 的完整对话正文</span>
                  <small><Icon name="keyboard" size={13} /> Ctrl+1 返回会话浏览 · 右键查看更多操作</small>
                </Empty>
              )}
            </section>
          </div>
        </section>

        <section className={`view ${tab === "terminals" ? "active" : ""}`} aria-hidden={tab !== "terminals"}>
          <TerminalWorkspace
            ref={terminalRef}
            active={tab === "terminals"}
            onRunningChange={setTerminalRunning}
            projectWorkDir={projectWorkDir}
            onProjectWorkDirChange={handleWorkDirChange}
          />
        </section>

        <section className={`view ${tab === "supervise" ? "active" : ""}`} aria-hidden={tab !== "supervise"}>
          <div className="supervise-closed-loop">
            <WorkspaceSidebar
              workspaces={workspaces.list}
              activeId={workspaces.activeId}
              onSelect={handleSelectWorkspace}
              onAdd={handleAddWorkspace}
              onRemove={handleRemoveWorkspace}
              onRename={handleRenameWorkspace}
            />
            <div className="supervise-closed-loop__main">
              {!activeWorkspace ? (
                <div className="empty-state">
                  <div className="empty-state__icon"><Icon name="folder-open" size={20} /></div>
                  <div className="empty-state__copy">
                    <strong>尚无工作空间</strong>
                    <span>点击左侧「+」添加一个项目目录，开始监督闭环。</span>
                  </div>
                </div>
              ) : (
                <>
                  <div className="supervise-tasks">
                    <div className="supervise-tasks__head">
                      <span className="eyebrow">ACTIVE</span>
                      <h4>任务记录</h4>
                      <span className="count-pill">{activeTasks.length}</span>
                    </div>
                    <TaskList
                      tasks={activeTasks}
                      onCancel={handleCancelTask}
                      onRetryReview={handleRetryReview}
                      selectedId={focusedTaskIdEffective}
                      onSelect={setFocusedTaskId}
                    />
                  </div>
                  <div
                    className={`supervise-layout ${compactSupervise ? "supervise-layout--stacked" : ""}`}
                    ref={superviseLayoutRef}
                  >
                    <div
                      className="panel-container"
                      style={compactSupervise ? { height: panelHeight } : { width: panelWidth }}
                    >
                      <SupervisePanel
                        workDir={projectWorkDir}
                        onWorkDirChange={handleWorkDirChange}
                        readOnly
                        onStarted={() => void loadTasks()}
                        onDriveStarted={handleDriveStarted}
                      />
                    </div>
                    <SplitHandle
                      orientation={compactSupervise ? "horizontal" : "vertical"}
                      label={compactSupervise ? "调整监督配置高度" : "调整监督配置宽度"}
                      value={compactSupervise ? panelHeight : panelWidth}
                      min={compactSupervise ? SUPERVISE_HEIGHT_MIN : SUPERVISE_WIDTH_MIN}
                      max={getSupervisePanelMax}
                      onChange={compactSupervise ? setPanelHeight : setPanelWidth}
                      className="supervise-split"
                      valueText={`监督配置${compactSupervise ? "高度" : "宽度"} ${Math.round(compactSupervise ? panelHeight : panelWidth)} 像素`}
                    />
                    <ReviewBoard
                      workDir={projectWorkDir}
                      taskId={focusedTaskIdEffective}
                      onViewSession={openTranscriptByFile}
                      active={tab === "supervise"}
                    />
                  </div>
                </>
              )}
            </div>
          </div>
        </section>

        <section className={`view ${tab === "mcp" ? "active" : ""}`} aria-hidden={tab !== "mcp"}>
          <MCPStatusPanel onHealthChange={handleMcpHealthChange} />
        </section>

        <StatusBar
          workspace={currentNav.label}
          claudeCount={claudeCount}
          codexCount={codexCount}
          terminalRunning={terminalRunning}
          superviseRunning={superviseRunning}
          mcpHealth={mcpHealth}
        />
      </div>
    </main>
  );
}

export default App;
