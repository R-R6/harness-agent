import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TerminalWorkspace } from "../TerminalWorkspace";
import { CODEX_CURSOR_PIN } from "../../lib/xtermRuntime";

const mocks = vi.hoisted(() => {
  const terminals: MockTerminal[] = [];

  class MockTerminal {
    cols = 100;
    rows = 30;
    writes: string[] = [];
    options: unknown;
    onDataHandler: ((data: string) => void) | null = null;
    dispose = vi.fn();
    reset = vi.fn(function (this: MockTerminal) {
      this.writes = [];
    });

    constructor(options?: unknown) {
      this.options = options ?? { cursorBlink: false };
      terminals.push(this);
    }

    parser = {
      registerCsiHandler: vi.fn(() => ({ dispose: vi.fn() })),
    };

    loadAddon() {}
    open() {}
    writeln(data: string) { this.writes.push(data); }
    write(data: string) { this.writes.push(data); }
    clear() {}
    onWriteParsed() { return { dispose: vi.fn() }; }
    onData(handler: (data: string) => void) {
      this.onDataHandler = handler;
      return { dispose: vi.fn() };
    }
  }

  class MockFitAddon {
    fit = vi.fn();
  }

  return {
    invoke: vi.fn(),
    listen: vi.fn(),
    dialogOpen: vi.fn(),
    loadRuntime: vi.fn(),
    listeners: new Map<string, Array<(event: { payload: unknown }) => void>>(),
    terminals,
    MockTerminal,
    MockFitAddon,
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.dialogOpen }));
vi.mock("../../lib/xtermRuntime", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../lib/xtermRuntime")>();
  return { ...actual, loadXtermRuntime: mocks.loadRuntime };
});

class MockResizeObserver {
  observe() {}
  disconnect() {}
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function emitEvent(event: string, payload: unknown) {
  for (const handler of [...(mocks.listeners.get(event) ?? [])]) {
    handler({ payload });
  }
}

describe("TerminalWorkspace", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.listen.mockReset();
    mocks.dialogOpen.mockReset();
    mocks.loadRuntime.mockReset();
    mocks.listeners.clear();
    mocks.terminals.splice(0);
    vi.stubGlobal("ResizeObserver", MockResizeObserver);
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", () => {});
    mocks.loadRuntime.mockResolvedValue({
      Terminal: mocks.MockTerminal,
      FitAddon: mocks.MockFitAddon,
    });
    mocks.listen.mockImplementation(async (event: string, handler: (payload: { payload: unknown }) => void) => {
      const bucket = mocks.listeners.get(event) ?? [];
      bucket.push(handler);
      mocks.listeners.set(event, bucket);
      return () => {
        mocks.listeners.set(
          event,
          (mocks.listeners.get(event) ?? []).filter((current) => current !== handler),
        );
      };
    });
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_terminal") {
        return Promise.resolve({
          id: "terminal-claude-1",
          agent: "claude",
          work_dir: "F:\\project\\workspace-side\\Harness_agent",
          status: "running",
        });
      }
      return Promise.resolve(undefined);
    });
  });

  it("按需启动本机 CLI，并在切换工作区时保留 xterm 实例和 PTY 会话", async () => {
    const { rerender } = render(<TerminalWorkspace active />);

    await waitFor(() => {
      expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled();
      expect(mocks.terminals).toHaveLength(2);
    });

    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("start_terminal", {
        request: expect.objectContaining({
          agent: "claude",
          cols: 100,
          rows: 30,
          work_dir: "F:\\project\\workspace-side\\Harness_agent",
        }),
      });
      expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
    });
    expect(mocks.invoke.mock.calls.some((call) => call[0] === "resize_terminal")).toBe(false);

    await waitFor(() => {
      expect(mocks.listeners.get("terminal-output")).toHaveLength(1);
    });
    emitEvent("terminal-output", { sessionId: "terminal-claude-1", data: "hello from CLI" });
    expect(mocks.terminals[0].writes.filter((chunk) => chunk.includes("hello from CLI"))).toEqual([
      "hello from CLI",
    ]);

    rerender(<TerminalWorkspace active={false} />);
    rerender(<TerminalWorkspace active />);

    expect(mocks.terminals).toHaveLength(2);
    expect(mocks.terminals.every((terminal) => terminal.dispose.mock.calls.length === 0)).toBe(true);
    expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
    expect(mocks.terminals[0].reset).toHaveBeenCalled();
  });

  it("为 PTY 托管的 TUI 关闭 convertEol，避免全屏界面叠成两层", async () => {
    render(<TerminalWorkspace active />);
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));
    expect(mocks.terminals[0].options).toEqual(expect.objectContaining({
      convertEol: false,
      cursorBlink: false,
      scrollback: 0,
    }));
  });

  it("只钉住 Codex 的光标，Claude 输出原样写入", async () => {
    mocks.invoke.mockImplementation((command: string, payload?: { request?: { agent: string } }) => {
      if (command === "start_terminal") {
        const agent = payload?.request?.agent ?? "claude";
        return Promise.resolve({
          id: `terminal-${agent}-1`,
          agent,
          work_dir: "F:\\project\\workspace-side\\Harness_agent",
          status: "running",
        });
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalWorkspace active />);
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })).toHaveLength(2));
    const startButtons = screen.getAllByRole("button", { name: "启动" });
    await userEvent.click(startButtons[0]);
    await userEvent.click(startButtons[1]);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "停止 Codex CLI" })).toBeInTheDocument();
    });

    emitEvent("terminal-output", { sessionId: "terminal-claude-1", data: "A\x1b[?25lB" });
    emitEvent("terminal-output", { sessionId: "terminal-codex-1", data: "A\x1b[?25hB" });
    expect(mocks.terminals[0].writes.at(-1)).toBe("A\x1b[?25lB");
    expect(mocks.terminals[1].writes.at(-1)).toBe(`AB${CODEX_CURSOR_PIN}`);
  });

  it("启动失败后恢复空闲提示，不留下空白终端", async () => {
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_terminal") {
        return Promise.reject(new Error("boom"));
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalWorkspace active />);
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));
    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);

    await waitFor(() => {
      expect(screen.getAllByRole("alert")[0]).toHaveTextContent("Error: boom");
    });
    expect(mocks.terminals[0].reset).toHaveBeenCalled();
    expect(mocks.terminals[0].writes.some((line) => line.includes("点击启动后"))).toBe(true);
  });

  it("Claude 与 Codex 终端之间提供可访问的拖动分隔线", async () => {
    render(<TerminalWorkspace active />);
    const splitter = await screen.findByRole("separator", { name: "调整 Claude 与 Codex 终端区域" });
    expect(splitter).toHaveAttribute("aria-orientation", "vertical");
    expect(splitter).toHaveAttribute("aria-valuenow", "50");
  });

  it("按输入顺序将键入数据写入同一个 PTY", async () => {
    const firstWrite = deferred<void>();
    const secondWrite = deferred<void>();
    let writeCount = 0;
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_terminal") {
        return Promise.resolve({
          id: "terminal-claude-1",
          agent: "claude",
          work_dir: "F:\\project\\workspace-side\\Harness_agent",
          status: "running",
        });
      }
      if (command === "write_terminal") {
        writeCount += 1;
        return writeCount === 1 ? firstWrite.promise : secondWrite.promise;
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalWorkspace active />);
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));
    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    await waitFor(() => expect(mocks.terminals[0].onDataHandler).not.toBeNull());

    mocks.terminals[0].onDataHandler?.("a");
    mocks.terminals[0].onDataHandler?.("\r");

    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("write_terminal", {
        sessionId: "terminal-claude-1",
        data: "a",
      });
    });
    expect(mocks.invoke).not.toHaveBeenCalledWith("write_terminal", {
      sessionId: "terminal-claude-1",
      data: "\r",
    });

    firstWrite.resolve();
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("write_terminal", {
        sessionId: "terminal-claude-1",
        data: "\r",
      });
    });
    secondWrite.resolve();
  });

  it("listen 尚未完成就卸载时，迟到的订阅仍会被注销", async () => {
    const outputReady = deferred<void>();
    mocks.listen.mockImplementation(async (event: string, handler: (payload: { payload: unknown }) => void) => {
      if (event === "terminal-output") {
        await outputReady.promise;
      }
      const bucket = mocks.listeners.get(event) ?? [];
      bucket.push(handler);
      mocks.listeners.set(event, bucket);
      return () => {
        mocks.listeners.set(
          event,
          (mocks.listeners.get(event) ?? []).filter((current) => current !== handler),
        );
      };
    });

    const { unmount } = render(<TerminalWorkspace active />);
    await waitFor(() => expect(mocks.listen).toHaveBeenCalled());
    expect(mocks.listeners.get("terminal-output") ?? []).toHaveLength(0);
    unmount();
    outputReady.resolve();

    await waitFor(() => {
      expect(mocks.listeners.get("terminal-output") ?? []).toHaveLength(0);
    });
  });

  it("每个 PTY 数据块只写入一次，不会因为重复订阅叠成两层", async () => {
    render(<TerminalWorkspace active />);
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));
    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
      expect(mocks.listeners.get("terminal-output")).toHaveLength(1);
    });

    emitEvent("terminal-output", { sessionId: "terminal-claude-1", data: "use one." });
    expect(mocks.terminals[0].writes.filter((chunk) => chunk.includes("use one."))).toEqual(["use one."]);
  });

  it("点击文件夹图标打开目录选择器，并只更新对应面板的工作目录", async () => {
    mocks.dialogOpen.mockResolvedValue("D:\\picked-project");
    render(<TerminalWorkspace active />);
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));

    await userEvent.click(screen.getByRole("button", { name: "选择 Claude CLI 工作目录" }));
    expect(mocks.dialogOpen).toHaveBeenCalledWith(
      expect.objectContaining({
        directory: true,
        multiple: false,
        defaultPath: "F:\\project\\workspace-side\\Harness_agent",
      }),
    );
    expect(screen.getByLabelText("Claude CLI 工作目录")).toHaveValue("D:\\picked-project");
    expect(screen.getByLabelText("Codex CLI 工作目录")).toHaveValue(
      "F:\\project\\workspace-side\\Harness_agent",
    );
  });

  it("目录选择器取消时保持当前工作目录不变", async () => {
    mocks.dialogOpen.mockResolvedValue(null);
    render(<TerminalWorkspace active />);
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));

    await userEvent.click(screen.getByRole("button", { name: "选择 Codex CLI 工作目录" }));
    expect(screen.getByLabelText("Codex CLI 工作目录")).toHaveValue(
      "F:\\project\\workspace-side\\Harness_agent",
    );
  });
});
