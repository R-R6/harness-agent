import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TaskList, formatTaskTime, TASK_STATUS_LABEL, canRetryReview } from "../TaskList";
import type { TaskInfo } from "../../types";

const makeTask = (overrides: Partial<TaskInfo> = {}): TaskInfo => ({
  id: "task-1",
  work_dir: "D:\\project",
  kind: "engine",
  status: "running",
  rounds: 2,
  last_reason: "",
  started_at_ms: Date.now(),
  ...overrides,
});

describe("formatTaskTime", () => {
  it("格式化毫秒时间戳为 MM/DD HH:MM", () => {
    const d = new Date(2026, 7, 24, 14, 30); // Aug 24, 14:30
    const result = formatTaskTime(d.getTime());
    expect(result).toMatch(/^08\/24 14:30$/);
  });
});

describe("TaskList", () => {
  it("空列表显示空态", () => {
    render(<TaskList tasks={[]} onCancel={vi.fn()} />);
    expect(screen.getByText("暂无任务记录。填写下方表单启动监督闭环。")).toBeInTheDocument();
  });

  it("渲染运行中任务的状态徽标 + 目录 + 轮数 + 时间 + 取消按钮", () => {
    const tasks = [makeTask()];
    render(<TaskList tasks={tasks} onCancel={vi.fn()} />);
    expect(screen.getByText("运行中")).toBeInTheDocument();
    expect(screen.getByText("project")).toBeInTheDocument();
    expect(screen.getByText(/2 轮/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /取消/ })).toBeInTheDocument();
  });

  it("终态任务不显示取消按钮", () => {
    const tasks = [makeTask({ status: "accepted" })];
    render(<TaskList tasks={tasks} onCancel={vi.fn()} />);
    expect(screen.getByText("已通过")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /取消/ })).not.toBeInTheDocument();
  });

  it("点击取消按钮调用 onCancel", async () => {
    const onCancel = vi.fn();
    const tasks = [makeTask({ id: "task-42" })];
    render(<TaskList tasks={tasks} onCancel={onCancel} />);
    await userEvent.click(screen.getByRole("button", { name: /取消/ }));
    expect(onCancel).toHaveBeenCalledWith("task-42");
  });

  it("显示 ps1 任务类型标签", () => {
    const tasks = [makeTask({ kind: "ps1" })];
    render(<TaskList tasks={tasks} onCancel={vi.fn()} />);
    expect(screen.getByText(/无头/)).toBeInTheDocument();
  });

  it("有 last_reason 时显示原因摘要", () => {
    const tasks = [makeTask({ status: "rejected", last_reason: "缺少输入校验" })];
    render(<TaskList tasks={tasks} onCancel={vi.fn()} />);
    expect(screen.getByText("缺少输入校验")).toBeInTheDocument();
  });

  it("所有状态标签有对应中文映射", () => {
    const statuses: TaskInfo["status"][] = ["running", "accepted", "rejected", "cancelled", "aborted"];
    for (const s of statuses) {
      expect(TASK_STATUS_LABEL[s]).toBeDefined();
    }
  });

  it("已中止且有 session_file 时显示重试审查并回调", async () => {
    const onRetry = vi.fn();
    const tasks = [
      makeTask({
        status: "aborted",
        kind: "engine",
        session_file: "/tmp/s.jsonl",
        last_reason: "审查失败：额度用尽",
      }),
    ];
    render(<TaskList tasks={tasks} onCancel={vi.fn()} onRetryReview={onRetry} />);
    const btn = screen.getByRole("button", { name: /重试审查/ });
    expect(btn).toBeInTheDocument();
    await userEvent.click(btn);
    expect(onRetry).toHaveBeenCalledWith("task-1");
  });

  it("已中止但无 session_file 时不显示重试审查", () => {
    const tasks = [makeTask({ status: "aborted", kind: "engine", session_file: undefined })];
    render(<TaskList tasks={tasks} onCancel={vi.fn()} onRetryReview={vi.fn()} />);
    expect(screen.queryByRole("button", { name: /重试审查/ })).not.toBeInTheDocument();
  });

  it("canRetryReview 仅对 engine+aborted+有会话 为真", () => {
    expect(
      canRetryReview(makeTask({ status: "aborted", kind: "engine", session_file: "/x" })),
    ).toBe(true);
    expect(canRetryReview(makeTask({ status: "aborted", kind: "ps1", session_file: "/x" }))).toBe(
      false,
    );
    expect(canRetryReview(makeTask({ status: "accepted", session_file: "/x" }))).toBe(false);
  });

  it("点击行选中任务；点取消不触发选中", async () => {
    const onCancel = vi.fn();
    const onSelect = vi.fn();
    const tasks = [
      makeTask({ id: "task-1", status: "running" }),
      makeTask({ id: "task-2", status: "running", work_dir: "D:\\other" }),
    ];
    render(<TaskList tasks={tasks} onCancel={onCancel} selectedId="task-1" onSelect={onSelect} />);
    await userEvent.click(screen.getByTitle("D:\\other"));
    expect(onSelect).toHaveBeenCalledWith("task-2");
    onSelect.mockClear();
    await userEvent.click(screen.getAllByRole("button", { name: /取消/ })[1]);
    expect(onCancel).toHaveBeenCalledWith("task-2");
    expect(onSelect).not.toHaveBeenCalled();
  });
});
