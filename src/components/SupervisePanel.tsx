import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { continueSuperviseTerminal, runSupervise, runSuperviseTerminal } from "../lib/api";
import { useAgentCatalog } from "../lib/useAgentCatalog";
import { Icon } from "./Icon";
import type { SuperviseRequest, TaskInfo, TaskStatus } from "../types";

interface Props {
  /** 工作目录（受控：由 App 持有的项目上下文，与 Claude 终端 pane 同源） */
  workDir: string;
  onWorkDirChange: (dir: string) => void;
  /** 只读模式（阶段 C）：目录 = 激活空间，只读展示；换目录=侧栏切空间 */
  readOnly?: boolean;
  /** 焦点任务：非空进入查看态（只读描述 + 日志），空则编辑态（表单） */
  focusedTask?: TaskInfo | null;
  /** 启动成功后回调（携带 task_id） */
  onStarted: (taskId: string) => void;
  /** 「再来一轮」：rejected 任务追加一轮完整闭环（回调用于刷新任务列表） */
  onContinue?: (taskId: string) => void;
  /** 终端驱动模式启动成功后回调（App 切到终端 tab） */
  onDriveStarted?: () => void;
  /** 驱动前准备工人 PTY（claude 可自动新建；其余 agent 需已有空闲 pane），返回 session id */
  prepareDriveTerminal?: (agentId: string, workDir: string) => Promise<string>;
}

const TASK_STATUS_LABEL: Record<TaskStatus, string> = {
  running: "运行中",
  accepted: "已通过",
  rejected: "未通过",
  cancelled: "已取消",
  aborted: "已中止",
};

/** 闭环启动器：焦点任务查看（描述/日志）或 任务表单 + 启动 */
export function SupervisePanel({
  workDir,
  onWorkDirChange,
  readOnly = false,
  focusedTask,
  onStarted,
  onContinue,
  onDriveStarted,
  prepareDriveTerminal,
}: Props) {
  const [task, setTask] = useState("");
  const [level, setLevel] = useState("L1");
  const [mock, setMock] = useState(true);
  const [driveTerminal, setDriveTerminal] = useState(false);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState("");
  const [continuing, setContinuing] = useState(false);
  const [continueError, setContinueError] = useState("");
  const logEndRef = useRef<HTMLDivElement>(null);
  // 多 Agent：注册表 + 角色选择（记忆上次选择）
  const catalog = useAgentCatalog();
  const [workerAgent, setWorkerAgent] = useState(
    () => localStorage.getItem("ha-worker-agent") || "claude",
  );
  const [reviewerAgent, setReviewerAgent] = useState(
    () => localStorage.getItem("ha-reviewer-agent") || "codex",
  );

  const workerOptions = catalog.filter((c) => c.can_work);
  const reviewerOptions = catalog.filter((c) => c.can_review);
  const selectWorker = (id: string) => {
    setWorkerAgent(id);
    localStorage.setItem("ha-worker-agent", id);
  };
  const selectReviewer = (id: string) => {
    setReviewerAgent(id);
    localStorage.setItem("ha-reviewer-agent", id);
  };

  // 运行中任务的日志实时增长时，滚动到底部
  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [focusedTask?.log]);

  const start = async () => {
    setError("");
    if (!task.trim()) {
      setError("任务描述不能为空");
      return;
    }
    if (!workDir.trim()) {
      setError("工作目录不能为空（可点浏览选择）");
      return;
    }
    setStarting(true);
    try {
      const dir = workDir.trim();
      const req: SuperviseRequest = {
        task: task.trim(),
        work_dir: dir,
        level,
        mock,
        worker_agent: workerAgent,
        reviewer_agent: reviewerAgent,
      };
      if (driveTerminal && prepareDriveTerminal) {
        req.terminal_session_id = await prepareDriveTerminal(workerAgent, dir);
        // 先切到终端，让 xterm 挂上并能收键；引擎会等信任对话框结束再注入。
        onDriveStarted?.();
      }
      const taskId = driveTerminal
        ? await runSuperviseTerminal(req)
        : await runSupervise(req);
      onStarted(taskId);
      setTask("");
    } catch (e) {
      setError(String(e));
    } finally {
      setStarting(false);
    }
  };

  /** 「再来一轮」：注入上轮审查意见，复用原 Claude 会话跑一轮完整闭环 */
  const continueRound = async () => {
    if (!focusedTask) return;
    setContinuing(true);
    setContinueError("");
    try {
      await continueSuperviseTerminal({
        task_id: focusedTask.id,
        work_dir: focusedTask.work_dir,
      });
      onContinue?.(focusedTask.id);
    } catch (e) {
      setContinueError(String(e));
    } finally {
      setContinuing(false);
    }
  };

  const browseDir = async () => {
    setError("");
    try {
      const dir = await open({
        directory: true,
        multiple: false,
        title: "选择项目工作目录",
      });
      if (typeof dir === "string") onWorkDirChange(dir);
    } catch {
      // 用户取消选择或无权限
    }
  };

  // 查看态：选中任务 → 只读描述 + 日志
  if (focusedTask) {
    return (
      <div className="supervise-panel">
        <div className="task-detail">
          <div className="task-detail__head">
            <span className={`task-badge task-badge--${focusedTask.status}`}>
              {TASK_STATUS_LABEL[focusedTask.status]}
            </span>
            {(focusedTask.status === "rejected" ||
              focusedTask.status === "aborted" ||
              focusedTask.status === "cancelled") && (
              <button
                type="button"
                className="task-continue__btn"
                onClick={() => void continueRound()}
                disabled={continuing}
                title={
                  focusedTask.status === "rejected"
                    ? "注入上轮审查意见，Claude 同会话继续落地并重新审查"
                    : "以原任务正文重新注入（中止/取消的任务，任务可能尚未执行过）"
                }
              >
                <Icon name="refresh" size={14} />
                {continuing ? "再来一轮…" : "再来一轮"}
              </button>
            )}
          </div>
          {continueError && <div className="error">{continueError}</div>}
          <pre className="task-detail__desc">{focusedTask.task}</pre>
        </div>
        {focusedTask.log.length > 0 ? (
          <div className="log-stream">
            {focusedTask.log.map((l, i) => (
              <div key={i} className={`log-line ${logClass(l)}`}>{l}</div>
            ))}
            <div ref={logEndRef} />
          </div>
        ) : (
          <div className="log-stream log-stream--empty">暂无日志</div>
        )}
      </div>
    );
  }

  return (
    <div className="supervise-panel">
      <div className="form">
        <label>
          任务描述
          <textarea
            value={task}
            onChange={(e) => setTask(e.currentTarget.value)}
            placeholder="例如：写一个计算器 calc.py，要带输入校验和测试"
            rows={3}
          />
        </label>
        <label>
          工作目录
          <div className="dir-row">
            <input
              value={workDir}
              onChange={(e) => onWorkDirChange(e.currentTarget.value)}
              placeholder="Claude 干活的项目目录（可点击浏览选择）"
              disabled={readOnly}
              readOnly={readOnly}
            />
            {!readOnly && (
              <button type="button" className="browse" onClick={browseDir} title="打开资源管理器选择目录">
                <Icon name="folder-open" size={14} /> 浏览
              </button>
            )}
          </div>
        </label>
        <div className="form-row">
          <label>
            被监督方
            <select
              value={workerAgent}
              onChange={(e) => selectWorker(e.currentTarget.value)}
              title="谁来干活：以交互终端被注入任务"
            >
              {(workerOptions.length ? workerOptions : [{ id: "claude", name: "Claude Code", installed: true, sessions_present: false, can_work: true, can_review: true }]).map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                  {c.installed ? "" : "（未检测到安装）"}
                </option>
              ))}
            </select>
          </label>
          <label title={workerAgent === reviewerAgent ? "同型号自审可能偏软，建议换一个 Agent 审查" : "审查者以无头模式运行，读取会话/工作区后给出验收结论"}>
            监督方
            <select
              value={reviewerAgent}
              onChange={(e) => selectReviewer(e.currentTarget.value)}
            >
              {(reviewerOptions.length ? reviewerOptions : [{ id: "codex", name: "Codex CLI", installed: true, sessions_present: false, can_work: true, can_review: true }]).map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                  {c.installed ? "" : "（未检测到安装）"}
                </option>
              ))}
            </select>
          </label>
          <label>
            分级
            <select value={level} onChange={(e) => setLevel(e.currentTarget.value)}>
              <option value="L0">L0 · 小活（1 轮快审）</option>
              <option value="L1">L1 · 默认（3 轮）</option>
              <option value="L2">L2 · 大活（5 轮强审）</option>
            </select>
          </label>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={mock}
              onChange={(e) => setMock(e.currentTarget.checked)}
            />
            模拟模式（不花钱）
          </label>
          <label className="checkbox" title="任务注入被监督方的终端 pane：驱动时自动准备对应 PTY，干活全程可见、可随时插手">
            <input
              type="checkbox"
              checked={driveTerminal}
              onChange={(e) => setDriveTerminal(e.currentTarget.checked)}
            />
            驱动终端
          </label>
        </div>
        {error && <div className="error">{error}</div>}
        <div className="form-actions">
          <button type="button" onClick={() => void start()} disabled={starting}>
            <Icon name="play" size={14} />
            {starting ? "启动中..." : "启动监督闭环"}
          </button>
        </div>
      </div>
    </div>
  );
}

function logClass(line: string): string {
  if (line.includes("[PASS]") || line.includes("验收通过")) return "log-pass";
  if (line.includes("[FAIL]")) return "log-fail";
  if (line.includes("[WARN]") || line.includes("[ERROR]")) return "log-warn";
  if (line.includes("[STEP]")) return "log-step";
  return "";
}
