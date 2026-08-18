import { formatFull } from "../lib/formatTime";
import type { TranscriptEntry } from "../types";
import { Icon } from "./Icon";

/** 会话正文：按消息类型渲染（user / assistant / title / usage），时间统一北京时间 */
export function TranscriptView({ entries }: { entries: TranscriptEntry[] }) {
  if (entries.length === 0) {
    return <div className="empty">无内容</div>;
  }
  return (
    <div className="transcript">
      {entries.map((e, i) => (
        <div key={i} className={`msg msg-${e.type}`}>
          <div className="msg-head">
            <Icon name={e.type === "user" ? "message" : e.type === "assistant" ? "spark" : "file"} size={13} />
            <span className={`badge badge-${e.type}`}>{roleLabel(e.type)}</span>
            {e.at && <span className="at">{formatFull(e.at)}</span>}
          </div>
          <pre>{e.text}</pre>
        </div>
      ))}
    </div>
  );
}

function roleLabel(type: string): string {
  if (type === "user") return "用户";
  if (type === "assistant") return "助手";
  if (type === "title") return "标题";
  if (type === "usage") return "用量";
  return type;
}
