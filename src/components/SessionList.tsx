import { useMemo } from "react";
import type { SessionInfo } from "../types";

interface Props {
  sessions: SessionInfo[];
  selectedFile: string | null;
  onSelect: (s: SessionInfo) => void;
}

/**
 * 会话列表：按 agent 分组（Claude Code / Codex 各一组，组标题带主题色徽章），
 * 组内保持返回顺序（按更新时间倒序），点击加载正文
 */
export function SessionList({ sessions, selectedFile, onSelect }: Props) {
  const groups = useMemo(() => {
    const map = new Map<string, SessionInfo[]>();
    for (const s of sessions) {
      const arr = map.get(s.agent) ?? [];
      arr.push(s);
      map.set(s.agent, arr);
    }
    return Array.from(map.entries());
  }, [sessions]);

  if (sessions.length === 0) {
    return <div className="empty">暂无会话</div>;
  }

  return (
    <div className="session-groups">
      {groups.map(([agent, list]) => (
        <div key={agent} className="session-group">
          <h3 className="group-title">
            <span className={`badge badge-${agent}`}>{list[0].agentLabel}</span>
            <span className="count">{list.length}</span>
          </h3>
          <ul className="session-list">
            {list.map((s) => (
              <li
                key={s.file}
                data-agent={s.agent}
                className={s.file === selectedFile ? "selected" : ""}
              >
                <button
                  type="button"
                  onClick={() => onSelect(s)}
                  title={s.file}
                >
                  <span className="time">{s.updated}</span>
                  <span className="file">{s.file.split(/[\\/]/).pop()}</span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}
