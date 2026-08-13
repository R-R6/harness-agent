import type { SessionInfo } from "../types";

interface Props {
  sessions: SessionInfo[];
  selectedFile: string | null;
  onSelect: (s: SessionInfo) => void;
}

/** 会话列表：agent 徽章 + 更新时间 + 文件名，点击加载正文 */
export function SessionList({ sessions, selectedFile, onSelect }: Props) {
  if (sessions.length === 0) {
    return <div className="empty">暂无会话</div>;
  }
  return (
    <ul className="session-list">
      {sessions.map((s) => (
        <li
          key={s.file}
          className={s.file === selectedFile ? "selected" : ""}
        >
          <button
            type="button"
            onClick={() => onSelect(s)}
            title={s.file}
          >
            <span className={`badge badge-${s.agent}`}>{s.agentLabel}</span>
            <span className="time">{s.updated}</span>
            <span className="file">{s.file.split(/[\\/]/).pop()}</span>
          </button>
        </li>
      ))}
    </ul>
  );
}
