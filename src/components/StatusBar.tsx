import { Icon } from "./Icon";

interface Props {
  workspace: string;
  claudeCount: number;
  codexCount: number;
  terminalRunning?: number;
  mcpHealth?: "checking" | "healthy" | "degraded";
}

export function StatusBar({
  workspace,
  claudeCount,
  codexCount,
  terminalRunning = 0,
  mcpHealth = "checking",
}: Props) {
  const healthLabel = mcpHealth === "healthy" ? "MCP 健康" : mcpHealth === "degraded" ? "MCP 需要处理" : "MCP 检查中";

  return (
    <footer className="status-bar" aria-label="工作台状态">
      <div className="status-bar__left">
        <span className={`status-indicator status-indicator--${mcpHealth}`} aria-hidden="true" />
        <span>{healthLabel}</span>
        <span className="status-divider" />
        <span className="status-workspace">{workspace}</span>
      </div>
      <div className="status-bar__right">
        {terminalRunning > 0 && (
          <span className="status-chip status-chip--running">
            <Icon name="terminal" size={13} /> {terminalRunning} terminal{terminalRunning > 1 ? "s" : ""}
          </span>
        )}
        <span className="status-chip status-chip--claude">
          <span className="agent-dot agent-dot--claude" /> Claude {claudeCount}
        </span>
        <span className="status-chip status-chip--codex">
          <span className="agent-dot agent-dot--codex" /> Codex {codexCount}
        </span>
        <span className="status-hint">Ctrl 1–4 switch</span>
      </div>
    </footer>
  );
}
