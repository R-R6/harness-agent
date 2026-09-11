import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { SupervisePanel } from "../SupervisePanel";
import type { TaskInfo } from "../../types";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  dialogOpen: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.dialogOpen }));

function makeTask(overrides: Partial<TaskInfo> = {}): TaskInfo {
  return {
    id: "task-1",
    work_dir: "D:\\work",
    task: "写一个计算器",
    kind: "engine",
    status: "accepted",
    rounds: 2,
    last_reason: "",
    log: [],
    mock: false,
    started_at_ms: Date.now(),
    ...overrides,
  };
}

/** 受控宿主：面板的 workDir 由父组件持有（与 App 中的用法一致） */
function renderPanel(
  initialDir = "D:\\work",
  extra: {
    onDriveStarted?: () => void;
    prepareDriveTerminal?: (workDir: string) => Promise<string>;
    focusedTask?: TaskInfo | null;
    onContinue?: (taskId: string) => void;
  } = {},
) {
  const onStarted = vi.fn();
  const onWorkDirChange = vi.fn();
  const Host = () => {
    const [dir, setDir] = useState(initialDir);
    const handleDirChange = (v: string) => {
      onWorkDirChange(v);
      setDir(v);
    };
    return (
      <SupervisePanel
        workDir={dir}
        onWorkDirChange={handleDirChange}
        onStarted={onStarted}
        focusedTask={extra.focusedTask}
        onDriveStarted={extra.onDriveStarted}
        prepareDriveTerminal={extra.prepareDriveTerminal}
        onContinue={extra.onContinue}
      />
    );
  };
  const view = render(<Host />);
  return { ...view, onStarted, onWorkDirChange };
}

describe("SupervisePanel", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.listen.mockReset();
    mocks.dialogOpen.mockReset();
    // listen 返回取消函数
    mocks.listen.mockResolvedValue(() => {});
  });

  it("渲染任务表单（任务/目录/分级/模拟开关）", () => {
    renderPanel();
    expect(screen.getByText("任务描述")).toBeInTheDocument();
    expect(screen.getByText("工作目录")).toBeInTheDocument();
    expect(screen.getByText("分级")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "启动监督闭环" })).toBeInTheDocument();
  });

  it("空任务点击启动 → 显示错误，不调用 invoke", async () => {
    renderPanel();
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    expect(screen.getByText("任务描述不能为空")).toBeInTheDocument();
    expect(mocks.invoke).not.toHaveBeenCalled();
  });

  it("填写任务 + 工作目录 → 启动调用 run_supervise", async () => {
    mocks.invoke.mockResolvedValue("task-123");
    renderPanel("F:\\preset-dir");
    await userEvent.type(screen.getByPlaceholderText(/写一个计算器/), "写个猜数字游戏");
    const dirInput = screen.getByPlaceholderText(/浏览选择/);
    await userEvent.clear(dirInput);
    await userEvent.type(dirInput, "D:\\work");
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    expect(mocks.invoke).toHaveBeenCalledWith("run_supervise", {
      request: {
        task: "写个猜数字游戏",
        work_dir: "D:\\work",
        level: "L1",
        mock: true,
      },
    });
  });

  it("启动成功即以 task_id 回调 onStarted", async () => {
    mocks.invoke.mockResolvedValue("task-7");
    const { onStarted } = renderPanel("D:\\initial");
    await userEvent.type(screen.getByPlaceholderText(/写一个计算器/), "任务A");
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    await waitFor(() => expect(mocks.invoke).toHaveBeenCalled());
    expect(onStarted).toHaveBeenCalledWith("task-7");
  });

  it("点📁浏览 → 打开目录选择器并填入选中的目录", async () => {
    mocks.dialogOpen.mockResolvedValue("D:\\my-project");
    renderPanel("D:\\before");
    await userEvent.click(screen.getByRole("button", { name: /浏览/ }));
    expect(mocks.dialogOpen).toHaveBeenCalledWith(
      expect.objectContaining({ directory: true }),
    );
    const dirInput = screen.getByPlaceholderText(/浏览选择/);
    expect(dirInput).toHaveValue("D:\\my-project");
  });

  it("目录选择器取消（返回 null）→ 不改变当前目录", async () => {
    mocks.dialogOpen.mockResolvedValue(null);
    renderPanel("D:\\keep");
    const dirInput = screen.getByPlaceholderText(/浏览选择/);
    const before = (dirInput as HTMLInputElement).value;
    await userEvent.click(screen.getByRole("button", { name: /浏览/ }));
    expect(dirInput).toHaveValue(before);
  });

  it("工作目录为空时提示且不提交", async () => {
    renderPanel("");
    await userEvent.type(screen.getByPlaceholderText(/写一个计算器/), "测试任务");
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    expect(screen.getByText(/工作目录不能为空/)).toBeInTheDocument();
    expect(mocks.invoke).not.toHaveBeenCalled();
  });

  it("mock 关闭时 request.mock=false", async () => {
    mocks.invoke.mockResolvedValue("task-1");
    renderPanel();
    await userEvent.type(screen.getByPlaceholderText(/写一个计算器/), "t");
    await userEvent.click(screen.getByLabelText("模拟模式（不花钱）"));
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    const req = mocks.invoke.mock.calls[0][1].request;
    expect(req.mock).toBe(false);
  });

  it("勾选「驱动 Claude 终端」→ 调用 run_supervise_terminal 并回调 onDriveStarted", async () => {
    mocks.invoke.mockResolvedValue("task-8");
    const onDriveStarted = vi.fn();
    const prepareDriveTerminal = vi.fn().mockResolvedValue("terminal-99");
    renderPanel("D:\\work", { onDriveStarted, prepareDriveTerminal });
    await userEvent.type(screen.getByPlaceholderText(/写一个计算器/), "任务B");
    await userEvent.click(screen.getByLabelText("驱动 Claude 终端"));
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    await waitFor(() => expect(prepareDriveTerminal).toHaveBeenCalledWith("D:\\work"));
    await waitFor(() => expect(onDriveStarted).toHaveBeenCalledTimes(1));
    expect(onDriveStarted.mock.invocationCallOrder[0]).toBeLessThan(
      mocks.invoke.mock.invocationCallOrder[0],
    );
    expect(mocks.invoke).toHaveBeenCalledWith("run_supervise_terminal", {
      request: {
        task: "任务B",
        work_dir: "D:\\work",
        level: "L1",
        mock: true,
        terminal_session_id: "terminal-99",
      },
    });
  });

  it("查看态：渲染只读描述与日志，不渲染表单", () => {
    renderPanel("D:\\work", {
      focusedTask: makeTask({ task: "写个爬虫", log: ["[PASS] 验收通过"] }),
    });
    expect(screen.getByText("写个爬虫")).toBeInTheDocument();
    expect(screen.getByText("[PASS] 验收通过")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "启动监督闭环" })).not.toBeInTheDocument();
  });

  it("查看态（未通过）：显示「再来一轮」，点击调用 continue_supervise_terminal 并回调 onContinue", async () => {
    mocks.invoke.mockResolvedValue("task-1");
    const onContinue = vi.fn();
    renderPanel("D:\\work", {
      focusedTask: makeTask({ status: "rejected", last_reason: "请修复 xxx" }),
      onContinue,
    });
    const btn = screen.getByRole("button", { name: "再来一轮" });
    expect(btn).toBeInTheDocument();
    expect(screen.getByText("未通过").closest(".task-detail__head")).toContainElement(btn);

    await userEvent.click(btn);
    expect(mocks.invoke).toHaveBeenCalledWith(
      "continue_supervise_terminal",
      expect.objectContaining({
        request: expect.objectContaining({ task_id: "task-1", work_dir: "D:\\work" }),
      }),
    );
    await waitFor(() => expect(onContinue).toHaveBeenCalledWith("task-1"));
  });

  it("查看态（已通过/运行中）：不显示「再来一轮」", () => {
    renderPanel("D:\\work", { focusedTask: makeTask({ status: "accepted" }) });
    expect(screen.queryByRole("button", { name: "再来一轮" })).not.toBeInTheDocument();
  });

  it("查看态（已中止/已取消）：也显示「再来一轮」（以原任务重启）", () => {
    const first = renderPanel("D:\\work", {
      focusedTask: makeTask({ status: "aborted", last_reason: "输入栏未就绪" }),
    });
    expect(screen.getByRole("button", { name: "再来一轮" })).toBeInTheDocument();
    first.unmount();
    renderPanel("D:\\work", {
      focusedTask: makeTask({ status: "cancelled", last_reason: "用户取消" }),
    });
    expect(screen.getByRole("button", { name: "再来一轮" })).toBeInTheDocument();
  });

  it("启动请求进行中时禁用启动按钮，防止连点", async () => {
    let resolveStart: ((id: string) => void) | undefined;
    mocks.invoke.mockImplementation(
      () =>
        new Promise<string>((resolve) => {
          resolveStart = resolve;
        }),
    );
    renderPanel("D:\\work");
    await userEvent.type(screen.getByPlaceholderText(/写一个计算器/), "任务A");
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    expect(await screen.findByRole("button", { name: "启动中..." })).toBeDisabled();
    resolveStart?.("task-1");
    expect(await screen.findByRole("button", { name: "启动监督闭环" })).toBeEnabled();
  });
});
