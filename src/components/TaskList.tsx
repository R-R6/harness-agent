import type { TaskInfo } from "../types";
import { basenameOf } from "../lib/workspaces";
import { Icon } from "./Icon";

interface Props {
  tasks: TaskInfo[];
  /** 取消运行中的任务（复用 cancel_supervise） */
  onCancel: (taskId: string) => void;
}

export const TASK_STATUS_LABEL: Record<TaskInfo["status"], string> = {
  running: "运行中",
  accepted: "已通过",
  rejected: "未通过",
  cancelled: "已取消",
  aborted: "已中止",
};

export const TASK_KIND_LABEL: Record<TaskInfo["kind"], string> = {
  engine: "终端驱动",
  ps1: "无头",
};

/** 毫秒时间戳 → "MM/DD HH:MM" 本地时间 */
export function formatTaskTime(ms: number): string {
  const d = new Date(ms);
  const month = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  const hour = String(d.getHours()).padStart(2, "0");
  const minute = String(d.getMinutes()).padStart(2, "0");
  return `${month}/${day} ${hour}:${minute}`;
}

/** 监督闭环任务列表：状态/轮数/目录/开始时间；运行中带「取消」 */
export function TaskList({ tasks, onCancel }: Props) {
  if (tasks.length === 0) {
    return (
      <div className="task-list task-list--empty">
        <span>暂无任务记录。填写下方表单启动监督闭环。</span>
      </div>
    );
  }
  return (
    <ul className="task-list">
      {tasks.map((t) => (
        <li key={t.id} className={`task-row task-row--${t.status}`} title={t.work_dir}>
          <span className={`task-badge task-badge--${t.status}`}>
            {TASK_STATUS_LABEL[t.status]}
          </span>
          <div className="task-row__meta">
            <div className="task-row__dir">
              <Icon name="folder" size={12} />
              <span>{basenameOf(t.work_dir)}</span>
            </div>
            <div className="task-row__sub">
              {TASK_KIND_LABEL[t.kind]} · {t.rounds} 轮 · {formatTaskTime(t.started_at_ms)}
              {t.last_reason && (
                <span className="task-row__reason" title={t.last_reason}>
                  {t.last_reason}
                </span>
              )}
            </div>
          </div>
          {t.status === "running" && (
            <button type="button" className="task-row__cancel" onClick={() => onCancel(t.id)}>
              <Icon name="stop" size={12} /> 取消
            </button>
          )}
        </li>
      ))}
    </ul>
  );
}