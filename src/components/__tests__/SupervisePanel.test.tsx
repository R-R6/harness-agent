import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { SupervisePanel } from "../SupervisePanel";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  dialogOpen: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.dialogOpen }));

/** 受控宿主：面板的 workDir 由父组件持有（与 App 中的用法一致） */
function renderPanel(initialDir = "D:\\work") {
  const onStarted = vi.fn();
  const onWorkDirChange = vi.fn();
  const Host = () => {
    const [dir, setDir] = useState(initialDir);
    const handleDirChange = (v: string) => {
      onWorkDirChange(v);
      setDir(v);
    };
    return <SupervisePanel workDir={dir} onWorkDirChange={handleDirChange} onStarted={onStarted} />;
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

  it("启动成功即以当前目录回调 onStarted（看板从运行起就指向正确目录）", async () => {
    mocks.invoke.mockResolvedValue("task-7");
    const { onStarted } = renderPanel("D:\\initial");
    await userEvent.type(screen.getByPlaceholderText(/写一个计算器/), "任务A");
    // 挂载后修改目录再启动：回调必须带新目录，而不是挂载时的旧值（stale 闭包回归）
    const dirInput = screen.getByPlaceholderText(/浏览选择/);
    await userEvent.clear(dirInput);
    await userEvent.type(dirInput, "D:\\changed");
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    await waitFor(() => expect(mocks.invoke).toHaveBeenCalled());
    expect(onStarted).toHaveBeenCalledWith("D:\\changed");
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

  it("done 事件带非 0 退出码 → 显示任务失败", async () => {
    mocks.invoke.mockResolvedValue("task-9");
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let doneHandler: any = null;
    mocks.listen.mockImplementation((ev: string, cb: (e: never) => void) => {
      if (ev === "supervise-done") {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        doneHandler = cb as any;
      }
      return Promise.resolve(() => {});
    });
    renderPanel("D:\\work");
    await userEvent.type(screen.getByPlaceholderText(/写一个计算器/), "任务A");
    await userEvent.click(screen.getByRole("button", { name: "启动监督闭环" }));
    await waitFor(() =>
      expect(mocks.invoke).toHaveBeenCalledWith("run_supervise", expect.anything()),
    );
    doneHandler?.({ payload: { taskId: "task-9", exitCode: 1 } });
    expect(await screen.findByText(/任务失败（退出码 1）/)).toBeInTheDocument();
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

  it("收到 supervise-log 事件 → 渲染日志行", async () => {
    // 捕获 listen 注册的回调
    let logHandler: ((e: { payload: { taskId: string; line: string } }) => void) | undefined;
    mocks.listen.mockImplementation((event: string, cb: (e: never) => void) => {
      if (event === "supervise-log") logHandler = cb as never;
      return Promise.resolve(() => {});
    });
    mocks.invoke.mockResolvedValue("task-1");

    renderPanel();
    // 等 useEffect 异步注册 listener 完成
    await waitFor(() => expect(logHandler).toBeDefined());
    // 触发一次日志事件
    logHandler?.({ payload: { taskId: "task-1", line: "[PASS] 验收通过" } });
    expect(await screen.findByText("[PASS] 验收通过")).toBeInTheDocument();
  });
});
