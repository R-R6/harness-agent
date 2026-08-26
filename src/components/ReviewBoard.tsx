import { useCallback, useEffect, useState } from "react";
import { fetchReviewArtifacts } from "../lib/api";
import type { ReviewArtifact } from "../types";
import { Icon } from "./Icon";

interface Props {
  workDir: string;
  /** 焦点任务：无头产物在 .supervise/tasks/<id>/；省略则读根 .supervise */
  taskId?: string | null;
  /** 点击「查看会话」跳到会话浏览打开该轮的 transcript（file 为空时按钮隐藏） */
  onViewSession?: (file: string) => void;
  /** 所在 tab 是否激活：切入时自动刷新（监督任务运行中逐轮落盘，切回即见
   *  最新卡片，不必手点刷新；默认 true，独立使用保持挂载即加载） */
  active?: boolean;
}

/** 审查看板：读 .supervise 产物，按轮次展示 verdict/reason */
export function ReviewBoard({ workDir, taskId, onViewSession, active = true }: Props) {
  const [artifacts, setArtifacts] = useState<ReviewArtifact[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    if (!workDir.trim()) return;
    setLoading(true);
    setError("");
    try {
      setArtifacts(await fetchReviewArtifacts(workDir.trim(), taskId));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [workDir, taskId]);

  // 挂载即加载 + workDir 变化加载 + tab 切回（active 翻 true）自动刷新
  useEffect(() => {
    if (active) load();
  }, [active, load]);

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
                {a.file && onViewSession && (
                  <button
                    type="button"
                    className="link-button"
                    onClick={() => onViewSession(a.file!)}
                    title={a.file}
                  >
                    查看会话
                  </button>
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
