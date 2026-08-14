import { useCallback, useEffect, useState } from "react";
import { SessionList } from "./components/SessionList";
import { TranscriptView } from "./components/TranscriptView";
import { SearchBox } from "./components/SearchBox";
import { SupervisePanel } from "./components/SupervisePanel";
import { ReviewBoard } from "./components/ReviewBoard";
import { MCPStatusPanel } from "./components/MCPStatusPanel";
import { fetchSessions, fetchTranscript, searchSessions } from "./lib/api";
import { formatFull } from "./lib/formatTime";
import type { SessionInfo, TranscriptEntry } from "./types";
import "./App.css";

type Tab = "sessions" | "supervise" | "mcp";

const NAV_ITEMS: { id: Tab; label: string; icon: string; key: string }[] = [
  { id: "sessions", label: "会话浏览", icon: "💬", key: "1" },
  { id: "supervise", label: "监督闭环", icon: "🛡️", key: "2" },
  { id: "mcp", label: "MCP 状态", icon: "🔌", key: "3" },
];

/** 空态（带图标） */
function Empty({ icon, children }: { icon: string; children: React.ReactNode }) {
  return (
    <div className="empty">
      <div className="empty-icon">{icon}</div>
      <div>{children}</div>
    </div>
  );
}

/** 列表骨架屏（加载时替代"加载中..."文字） */
function SkeletonList() {
  return (
    <div className="skeleton">
      {Array.from({ length: 8 }).map((_, i) => (
        <div key={i}>
          <div className="skeleton-line" />
          <div className="skeleton-line short" />
        </div>
      ))}
    </div>
  );
}

function App() {
  const [tab, setTab] = useState<Tab>("sessions");

  // ---- 会话浏览状态（常驻 App，切 tab 不丢失）----
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selected, setSelected] = useState<SessionInfo | null>(null);
  const [transcript, setTranscript] = useState<TranscriptEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadingTrans, setLoadingTrans] = useState(false);
  const [error, setError] = useState("");
  const [searchKeyword, setSearchKeyword] = useState("");

  // ---- 监督闭环状态（常驻 App）----
  const [superviseWorkDir, setSuperviseWorkDir] = useState(
    "F:\\project\\workspace-side\\Harness_agent",
  );

  // ---- 可调面板宽度（可拖动分割线）----
  const [navWidth, setNavWidth] = useState(168);
  const [sidebarWidth, setSidebarWidth] = useState(300);
  const [panelWidth, setPanelWidth] = useState(400);

  /** 拖动分割线调整宽度（min/max 可传） */
  const startResize = (
    e: React.MouseEvent,
    setter: (w: number) => void,
    initial: number,
    min = 200,
    max = 560,
  ) => {
    e.preventDefault();
    const startX = e.clientX;
    const onMove = (ev: MouseEvent) => {
      const w = Math.min(Math.max(initial + (ev.clientX - startX), min), max);
      setter(w);
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
    };
    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  // 键盘快捷键：Ctrl+1 会话浏览 / Ctrl+2 监督闭环
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey) {
        if (e.key === "1") setTab("sessions");
        if (e.key === "2") setTab("supervise");
        if (e.key === "3") setTab("mcp");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const loadSessions = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setSessions(await fetchSessions());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadSessions();
  }, [loadSessions]);

  const selectSession = useCallback(async (s: SessionInfo) => {
    setSelected(s);
    setLoadingTrans(true);
    setError("");
    try {
      setTranscript(await fetchTranscript(s.file));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoadingTrans(false);
    }
  }, []);

  const handleSearch = useCallback(async (kw: string) => {
    setSearchKeyword(kw);
    setLoading(true);
    setError("");
    try {
      setSessions(await searchSessions(kw));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const handleClear = useCallback(() => {
    setSearchKeyword("");
    loadSessions();
  }, [loadSessions]);

  const handleSuperviseStarted = useCallback((workDir: string) => {
    setSuperviseWorkDir(workDir);
  }, []);

  const claudeCount = sessions.filter((s) => s.agent === "claude").length;
  const codexCount = sessions.filter((s) => s.agent === "codex").length;

  return (
    <main className="app">
      {/* 左侧导航（宽度可拖） */}
      <nav className="sidebar-nav" style={{ width: navWidth }}>
        <div className="nav-logo">
          <span className="logo-mark">H</span>
          <span>Harness Agent</span>
        </div>
        {NAV_ITEMS.map((item) => (
          <button
            key={item.id}
            type="button"
            className={`nav-item ${tab === item.id ? "active" : ""}`}
            onClick={() => setTab(item.id)}
            aria-label={item.label}
            title={`${item.label}（Ctrl+${item.key}）`}
          >
            <span className="nav-icon">{item.icon}</span>
            <span>{item.label}</span>
          </button>
        ))}
        <div className="nav-spacer" />
        <div className="nav-status">
          <div className="status-row">
            <span className="status-dot" />
            <span>MCP 已连接</span>
          </div>
          <div className="status-row">
            <span style={{ color: "var(--claude)" }}>●</span>
            <span>Claude {claudeCount}</span>
          </div>
          <div className="status-row">
            <span style={{ color: "var(--codex)" }}>●</span>
            <span>Codex {codexCount}</span>
          </div>
        </div>
      </nav>

      {/* 导航栏与内容区之间的可拖分割线 */}
      <div
        className="resizer"
        onMouseDown={(e) => startResize(e, setNavWidth, navWidth, 140, 280)}
      />

      {/* 主区域 */}
      <div className="main">
        <header className="topbar">
          <h2>{NAV_ITEMS.find((n) => n.id === tab)?.label}</h2>
          {tab === "sessions" && (
            <SearchBox onSearch={handleSearch} onClear={handleClear} />
          )}
        </header>

        {error && <div className="error">{error}</div>}

        {/* 双视图常驻（CSS 显隐切换，组件不卸载 → 状态不丢失） */}
        <section className={`view ${tab === "sessions" ? "active" : ""}`}>
          <div className="layout">
            <aside className="sidebar" style={{ width: sidebarWidth }}>
              {searchKeyword && (
                <div className="search-banner">
                  <span>
                    搜索 “{searchKeyword}” · {sessions.length} 条
                  </span>
                  <button type="button" onClick={handleClear}>
                    ✕ 清除
                  </button>
                </div>
              )}
              {loading && sessions.length === 0 ? (
                <SkeletonList />
              ) : (
                <SessionList
                  sessions={sessions}
                  selectedFile={selected?.file ?? null}
                  onSelect={selectSession}
                />
              )}
            </aside>
            <div
              className="resizer"
              onMouseDown={(e) => startResize(e, setSidebarWidth, sidebarWidth)}
            />
            <section className="detail">
              {selected ? (
                <>
                  <h2>
                    {selected.agentLabel} · {formatFull(selected.updated)}
                  </h2>
                  <p className="file-path" title={selected.file}>
                    {selected.file}
                  </p>
                  {loadingTrans && (
                    <div className="loading-bar">正在加载正文...</div>
                  )}
                  <TranscriptView entries={transcript} />
                </>
              ) : (
                <Empty icon="📄">
                  从左侧选择会话查看正文
                  <br />
                  <small>快捷键 Ctrl+1 / Ctrl+2 切换视图</small>
                </Empty>
              )}
            </section>
          </div>
        </section>

        <section className={`view ${tab === "supervise" ? "active" : ""}`}>
          <div className="supervise-layout">
            <div className="panel-container" style={{ width: panelWidth }}>
              <SupervisePanel onStarted={handleSuperviseStarted} />
            </div>
            <div
              className="resizer"
              onMouseDown={(e) => startResize(e, setPanelWidth, panelWidth)}
            />
            <ReviewBoard workDir={superviseWorkDir} />
          </div>
        </section>

        <section className={`view ${tab === "mcp" ? "active" : ""}`}>
          <MCPStatusPanel />
        </section>
      </div>
    </main>
  );
}

export default App;
