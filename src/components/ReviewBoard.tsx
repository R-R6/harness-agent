import { useCallback, useEffect, useState } from "react";
import { fetchReviewArtifacts } from "../lib/api";
import type { ReviewArtifact } from "../types";
import { Icon } from "./Icon";

interface Props {
  workDir: string;
}

/** 审查看板：读 .supervise 产物，按轮次展示 verdict/reason */
export function ReviewBoard({ workDir }: Props) {
  const [artifacts, setArtifacts] = useState<ReviewArtifact[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    if (!workDir.trim()) return;
    setLoading(true);
    setError("");
    try {
      setArtifacts(await fetchReviewArtifacts(workDir.trim()));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [workDir]);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div className="review-board">
      <div className="board-head">
        <h3>审查看板</h3>
        <button type="button" onClick={load} disabled={loading}>
          <Icon name="refresh" size={13} className={loading ? "spin" : ""} /> {loading ? "加载中..." : "刷新"}
        </button>
      </div>
      {error && <div className="error">{error}</div>}
      {artifacts.length === 0 ? (
        <div className="empty">
          暂无审查记录。启动监督闭环后，每轮审查意见会落盘到
          <code>工作目录\.supervise\</code> 并显示在这里。
        </div>
      ) : (
        <div className="rounds">
          {artifacts.map((a) => (
            <div
              key={a.round}
              className={`round-card round-${a.verdict.toLowerCase()}`}
            >
              <div className="round-head">
                <span className="round-no">第 {a.round} 轮</span>
                <span
                  className={`verdict verdict-${a.verdict.toLowerCase()}`}
                >
                  <Icon name={a.verdict === "PASS" ? "check" : "refresh"} size={12} />
                  {a.verdict === "PASS" ? "通过" : "需返工"}
                </span>
                {a.model && <span className="model">{a.model}</span>}
                {a.session_id && (
                  <span className="session-id">{a.session_id}</span>
                )}
              </div>
              {a.reason && <pre className="reason">{a.reason}</pre>}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
