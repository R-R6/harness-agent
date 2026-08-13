import { useCallback, useEffect, useState } from "react";
import { SessionList } from "./components/SessionList";
import { TranscriptView } from "./components/TranscriptView";
import { SearchBox } from "./components/SearchBox";
import { SupervisePanel } from "./components/SupervisePanel";
import { ReviewBoard } from "./components/ReviewBoard";
import { fetchSessions, fetchTranscript, searchSessions } from "./lib/api";
import { formatFull } from "./lib/formatTime";
import type { SessionInfo, TranscriptEntry } from "./types";
import "./App.css";

type Tab = "sessions" | "supervise";

function App() {
  const [tab, setTab] = useState<Tab>("sessions");

  // ---- 会话浏览状态（常驻 App，切 tab 不丢失）----
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selected, setSelected] = useState<SessionInfo | null>(null);
  const [transcript, setTranscript] = useState<TranscriptEntry[]>([]);
  const [loading, setLoading] = useState(false); // 列表加载中
  const [loadingTrans, setLoadingTrans] = useState(false); // 正文加载中
  const [error, setError] = useState("");
  const [searchKeyword, setSearchKeyword] = useState("");

  // ---- 监督闭环状态（常驻 App）----
  const [superviseWorkDir, setSuperviseWorkDir] = useState(
    "F:\\project\\workspace-side\\Harness_agent",
  );

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

  // 切换会话：保留旧正文 + 顶部 loading 指示，新数据到了再替换（不闪空）
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

  // 搜索：设置搜索态，列表变搜索结果；保留 selected（用户可继续看详情）
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

  // 清除搜索：回到全部会话
  const handleClear = useCallback(() => {
    setSearchKeyword("");
    loadSessions();
  }, [loadSessions]);

  const handleSuperviseStarted = useCallback((workDir: string) => {
    setSuperviseWorkDir(workDir);
  }, []);

  return (
    <main className="app">
      <header className="app-header">
        <h1>Harness Agent</h1>
        <nav className="tabs">
          <button
            type="button"
            className={tab === "sessions" ? "active" : ""}
            onClick={() => setTab("sessions")}
          >
            会话浏览
          </button>
          <button
            type="button"
            className={tab === "supervise" ? "active" : ""}
            onClick={() => setTab("supervise")}
          >
            监督闭环
          </button>
        </nav>
        {tab === "sessions" && (
          <SearchBox onSearch={handleSearch} onClear={handleClear} />
        )}
      </header>

      {error && <div className="error">{error}</div>}

      {/* 双视图常驻（CSS 显隐切换），组件不卸载 → 日志流/看板数据不丢失 */}
      <section className={`view ${tab === "sessions" ? "active" : ""}`}>
        <div className="layout">
          <aside className="sidebar">
            {searchKeyword && (
              <div className="search-banner">
                <span>
                  搜索 “{searchKeyword}” 的结果（{sessions.length} 条）
                </span>
                <button type="button" onClick={handleClear}>
                  ✕ 清除搜索
                </button>
              </div>
            )}
            {loading && sessions.length === 0 ? (
              <div className="empty">加载中...</div>
            ) : (
              <SessionList
                sessions={sessions}
                selectedFile={selected?.file ?? null}
                onSelect={selectSession}
              />
            )}
          </aside>
          <section className="detail">
            {selected ? (
              <>
                <h2>
                  {selected.agentLabel} · {formatFull(selected.updated)}
                </h2>
                <p className="file-path" title={selected.file}>
                  {selected.file}
                </p>
                {loadingTrans && <div className="loading-bar">正在加载正文...</div>}
                <TranscriptView entries={transcript} />
              </>
            ) : (
              <div className="empty">从左侧选择会话查看正文</div>
            )}
          </section>
        </div>
      </section>

      <section className={`view ${tab === "supervise" ? "active" : ""}`}>
        <div className="supervise-layout">
          <SupervisePanel onStarted={handleSuperviseStarted} />
          <ReviewBoard workDir={superviseWorkDir} />
        </div>
      </section>
    </main>
  );
}

export default App;
