import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { listenWhileMounted } from "../lib/listenWhileMounted";
import { runSupervise, runSuperviseTerminal } from "../lib/api";
import { Icon } from "./Icon";

interface Props {
  /** 工作目录（受控：由 App 持有的项目上下文，与 Claude 终端 pane 同源） */
  workDir: string;
  onWorkDirChange: (dir: string) => void;
  /** 只读模式（阶段 C）：目录 = 激活空间，只读展示；换目录=侧栏切空间 */
  readOnly?: boolean;
  /** 启动成功后回调（携带启动时的目录，供审看看板定位 .supervise 产物） */
  onStarted: (workDir: string) => void;
  /** 终端驱动模式启动成功后回调（App 切到终端 tab 让用户看到干活过程） */
  onDriveStarted?: () => void;
}

interface LogLine {
  taskId: string;
  line: string;
}

/** 闭环启动器：任务表单 + 启动 + 最近一次任务的实时日志流 */
export function SupervisePanel({ workDir, onWorkDirChange, readOnly = false, onStarted, onDriveStarted }: Props) {
  const [task, setTask] = useState("");
  const [level, setLevel] = useState("L1");
  const [mock, setMock] = useState(true);
  const [driveTerminal, setDriveTerminal] = useState(false);
  const [starting, setStarting] = useState(false);
  const [logs, setLogs] = useState<LogLine[]>([]);
  const [error, setError] = useState("");
  const logEndRef = useRef<HTMLDivElement>(null);
  const lastTaskIdRef = useRef<string | null>(null);

  useEffect(() => {
    const stopLog = listenWhileMounted<LogLine>("supervise-log", (e) => {
      if (e.payload.taskId !== lastTaskIdRef.current) return;
      setLogs((prev) => [...prev.slice(-499), e.payload]);
    });
    const stopDone = listenWhileMounted<{ taskId: string; exitCode?: number | null }>(
      "supervise-done",
      (e) => {
        if (e.payload.taskId !== lastTaskIdRef.current) return;
        const code = e.payload.exitCode;
        if (code != null && code !== 0) {
          setError(`任务失败（退出码 ${code}），详见下方日志`);
        }
      },
    );
    return () => {
      stopLog();
      stopDone();
    };
  }, []);

  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [logs]);

  useEffect(() => {
    setLogs([]);
    lastTaskIdRef.current = null;
    setError("");
  }, [workDir]);

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
      const req = {
        task: task.trim(),
        work_dir: workDir.trim(),
        level,
        mock,
      };
      const taskId = driveTerminal
        ? await runSuperviseTerminal(req)
        : await runSupervise(req);
      lastTaskIdRef.current = taskId;
      setLogs([]);
      onStarted(workDir.trim());
      if (driveTerminal) onDriveStarted?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setStarting(false);
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
          <label className="checkbox" title="任务注入运行中的 Claude 终端 pane：干活全程可见、可随时插手；需先在终端工作台以该目录启动 Claude CLI">
            <input
              type="checkbox"
              checked={driveTerminal}
              onChange={(e) => setDriveTerminal(e.currentTarget.checked)}
            />
            驱动 Claude 终端
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
