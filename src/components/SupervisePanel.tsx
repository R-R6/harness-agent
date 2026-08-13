import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { cancelSupervise, runSupervise } from "../lib/api";

interface Props {
  /** 启动后回调（父组件刷新审查看板） */
  onStarted: (workDir: string) => void;
}

interface LogLine {
  taskId: string;
  line: string;
}

/** 闭环启动器：任务表单 + 启动/取消 + 实时日志流 */
export function SupervisePanel({ onStarted }: Props) {
  const [task, setTask] = useState("");
  const [workDir, setWorkDir] = useState("F:\\project\\workspace-side\\Harness_agent");
  const [level, setLevel] = useState("L1");
  const [mock, setMock] = useState(true);
  const [runningTask, setRunningTask] = useState<string | null>(null);
  const [logs, setLogs] = useState<LogLine[]>([]);
  const [error, setError] = useState("");
  const logEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let unLog: UnlistenFn | undefined;
    let unDone: UnlistenFn | undefined;
    (async () => {
      unLog = await listen<LogLine>("supervise-log", (e) => {
        setLogs((prev) => [...prev.slice(-499), e.payload]); // 最多 500 行
      });
      unDone = await listen<{ taskId: string }>("supervise-done", (e) => {
        setRunningTask((cur) => (cur === e.payload.taskId ? null : cur));
        onStarted(workDir);
      });
    })();
    return () => {
      unLog?.();
      unDone?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [logs]);

  const start = async () => {
    setError("");
    if (!task.trim()) {
      setError("任务描述不能为空");
      return;
    }
    try {
      const taskId = await runSupervise({
        task: task.trim(),
        work_dir: workDir.trim(),
        level,
        mock,
      });
      setRunningTask(taskId);
      setLogs([]);
    } catch (e) {
      setError(String(e));
    }
  };

  const cancel = async () => {
    if (!runningTask) return;
    try {
      await cancelSupervise(runningTask);
    } catch (e) {
      setError(String(e));
    }
  };

  // 打开系统目录选择器选工作目录
  const browseDir = async () => {
    setError("");
    try {
      const dir = await open({
        directory: true,
        multiple: false,
        title: "选择项目工作目录",
      });
      if (typeof dir === "string") setWorkDir(dir);
    } catch (e) {
      setError(String(e));
    }
  };

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
              onChange={(e) => setWorkDir(e.currentTarget.value)}
              placeholder="Claude 干活的项目目录（可点击浏览选择）"
            />
            <button type="button" className="browse" onClick={browseDir} title="打开资源管理器选择目录">
              📁 浏览
            </button>
          </div>
        </label>
        <div className="form-row">
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
        </div>
        {error && <div className="error">{error}</div>}
        <div className="form-actions">
          {runningTask ? (
            <button type="button" className="danger" onClick={cancel}>
              取消任务
            </button>
          ) : (
            <button type="button" onClick={start}>
              启动监督闭环
            </button>
          )}
        </div>
      </div>

      {logs.length > 0 && (
        <div className="log-stream">
          {logs.map((l, i) => (
            <div key={i} className={`log-line ${logClass(l.line)}`}>
              {l.line}
            </div>
          ))}
          <div ref={logEndRef} />
        </div>
      )}
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
