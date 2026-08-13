import { useCallback, useEffect, useState } from "react";
import { SessionList } from "./components/SessionList";
import { TranscriptView } from "./components/TranscriptView";
import { SearchBox } from "./components/SearchBox";
import { SupervisePanel } from "./components/SupervisePanel";
import { ReviewBoard } from "./components/ReviewBoard";
import { fetchSessions, fetchTranscript, searchSessions } from "./lib/api";
import type { SessionInfo, TranscriptEntry } from "./types";
import "./App.css";

type Tab = "sessions" | "supervise";

function App() {
  const [tab, setTab] = useState<Tab>("sessions");

  // ---- 会话浏览状态 ----
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selected, setSelected] = useState<SessionInfo | null>(null);
  const [transcript, setTranscript] = useState<TranscriptEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  // ---- 监督闭环状态 ----
  const [superviseWorkDir, setSuperviseWorkDir] = useState(
    "F:\\project\\workspace-side\\mcp-lab",
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

  const selectSession = useCallback(async (s: SessionInfo) => {
    setSelected(s);
    setTranscript([]);
    setError("");
    try {
      setTranscript(await fetchTranscript(s.file));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const handleSearch = useCallback(async (kw: string) => {
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

      {tab === "sessions" ? (
        <>
          {error && <div className="error">{error}</div>}
          <div className="layout">
            <aside className="sidebar">
              {loading ? (
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
                    {selected.agentLabel} · {selected.updated}
                  </h2>
                  <p className="file-path" title={selected.file}>
                    {selected.file}
                  </p>
                  <TranscriptView entries={transcript} />
                </>
              ) : (
                <div className="empty">从左侧选择会话查看正文</div>
              )}
            </section>
          </div>
        </>
      ) : (
        <div className="supervise-layout">
          <SupervisePanel onStarted={handleSuperviseStarted} />
          <ReviewBoard workDir={superviseWorkDir} />
        </div>
      )}
    </main>
  );
}

export default App;
