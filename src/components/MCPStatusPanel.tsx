import { useCallback, useEffect, useState } from "react";
import { checkMcp, fixMcp } from "../lib/api";
import type { McpFixResult, McpStatus } from "../types";
import { Icon } from "./Icon";

/**
 * MCP 状态面板：agent-sessions MCP 注册健康检查 + 一键修复。
 * 背景：Codex 桌面端会重写 ~/.codex/config.toml 冲掉注册（codex++ #353），
 * 本面板让你随时确认注册还在、被冲则一键恢复。
 */
interface Props {
  onHealthChange?: (health: "checking" | "healthy" | "degraded") => void;
}

export function MCPStatusPanel({ onHealthChange }: Props) {
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [fixing, setFixing] = useState(false);
  const [fixResult, setFixResult] = useState<McpFixResult | null>(null);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    setFixResult(null);
    onHealthChange?.("checking");
    try {
      const next = await checkMcp();
      setStatus(next);
      onHealthChange?.(next.items.every((item) => item.ok) && next.handshake_ok ? "healthy" : "degraded");
    } catch (e) {
      setError(String(e));
      onHealthChange?.("degraded");
    } finally {
      setLoading(false);
    }
  }, [onHealthChange]);

  useEffect(() => {
    load();
  }, [load]);

  const fix = async () => {
    setFixing(true);
    setError("");
    try {
      const r = await fixMcp();
      setFixResult(r);
      // 修复后重查
      const next = await checkMcp();
      setStatus(next);
      onHealthChange?.(next.items.every((item) => item.ok) && next.handshake_ok ? "healthy" : "degraded");
    } catch (e) {
      setError(String(e));
      onHealthChange?.("degraded");
    } finally {
      setFixing(false);
    }
  };

  const failed = status?.items.filter((i) => !i.ok).length ?? 0;
  const allOk = status ? failed === 0 : false;

  return (
    <div className="mcp-panel">
      <div className="board-head">
        <h3>agent-sessions MCP 注册健康检查</h3>
        <button onClick={load} disabled={loading}>
          <Icon name="refresh" size={13} className={loading ? "spin" : ""} /> {loading ? "检查中..." : "重新检查"}
        </button>
        {!allOk && status && (
          <button className="fix" onClick={fix} disabled={fixing}>
            <Icon name="shield" size={13} /> {fixing ? "修复中..." : "一键修复"}
          </button>
        )}
      </div>

      {error && <div className="error">{error}</div>}

      {fixResult && (
        <div className={`fix-result ${fixResult.ok ? "ok" : "noop"}`}>
          <Icon name={fixResult.ok ? "check" : "activity"} size={15} />
          <strong>{fixResult.ok ? "已修复" : "无需修复"}</strong>
          <span>{fixResult.message}</span>
          {fixResult.backup_path && (
            <span className="backup">备份：{fixResult.backup_path}</span>
          )}
        </div>
      )}

      {!status && !loading && !error && (
        <div className="empty">加载检查结果...</div>
      )}

      {status && (
        <>
          <div className={`summary ${allOk ? "ok" : "bad"}`}>
            {allOk ? (
              <><Icon name="check" size={15} /> MCP 注册健康（{status.items.length} 项全过）</>
            ) : (
              <><Icon name="alert" size={15} /> 发现 {failed} 项异常，点击“一键修复”自动恢复</>
            )}
            <span className="cfg">config: {status.config_path}</span>
          </div>

          <ul className="mcp-items">
            {status.items.map((it) => (
              <li key={it.name} className={it.ok ? "ok" : "bad"}>
                <span className="mark"><Icon name={it.ok ? "check" : "x"} size={13} /></span>
                <span className="name">{it.name}</span>
                <span className="detail">{it.detail}</span>
              </li>
            ))}
            <li className={status.handshake_ok ? "ok" : "bad"}>
              <span className="mark"><Icon name={status.handshake_ok ? "check" : "x"} size={13} /></span>
              <span className="name">真实握手（spawn node server.js → initialize）</span>
              <span className="detail">
                {status.handshake_ok ? "MCP 服务器响应正常" : "握手失败，检查 node 与 server.js"}
              </span>
            </li>
          </ul>
        </>
      )}
    </div>
  );
}
