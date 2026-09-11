import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useEffect, useState } from "react";
import { TerminalWorkspace, type TerminalWorkspaceHandle } from "../TerminalWorkspace";
import { CODEX_CURSOR_PIN } from "../../lib/xtermRuntime";

/** 与 App 中一致的受控宿主：Claude pane 的项目目录由父组件持有 */
const PROJECT_DIR = "F:\\project\\workspace-side\\Harness_agent";

function WorkspaceHost({ active = true, projectDir = PROJECT_DIR }: { active?: boolean; projectDir?: string }) {
  const [dir, setDir] = useState(projectDir);
  useEffect(() => {
    setDir(projectDir);
  }, [projectDir]);
  return (
    <TerminalWorkspace active={active} projectWorkDir={dir} onProjectWorkDirChange={setDir} />
  );
}

function renderHost(active = true, projectDir = PROJECT_DIR) {
  return render(<WorkspaceHost active={active} projectDir={projectDir} />);
}

const mocks = vi.hoisted(() => {
  const terminals: MockTerminal[] = [];

  class MockTerminal {
    cols = 100;
    rows = 30;
    writes: string[] = [];
    options: unknown;
    onDataHandler: ((data: string) => void) | null = null;
    focus = vi.fn();
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
    // Codex pane 的目录是组件内 localStorage 持久化状态，预填与旧默认值一致
    localStorage.setItem("ha-workdir-codex", PROJECT_DIR);
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

  afterEach(() => {
    localStorage.removeItem("ha-workdir-codex");
  });

  it("按需启动本机 CLI，并在切换工作区时保留 xterm 实例和 PTY 会话", async () => {
    const { rerender } = renderHost();

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
    // 启动后会按实际 xterm 尺寸再 sync 一次 resize（修黑屏）
    await waitFor(() => {
      expect(mocks.invoke.mock.calls.some((call) => call[0] === "resize_terminal")).toBe(true);
    });

    await waitFor(() => {
      expect(mocks.listeners.get("terminal-output")).toHaveLength(1);
    });
    emitEvent("terminal-output", { sessionId: "terminal-claude-1", data: "hello from CLI" });
    expect(mocks.terminals[0].writes.filter((chunk) => chunk.includes("hello from CLI"))).toEqual([
      "hello from CLI",
    ]);

    rerender(<WorkspaceHost active={false} />);
    rerender(<WorkspaceHost active />);

    expect(mocks.terminals).toHaveLength(2);
    expect(mocks.terminals.every((terminal) => terminal.dispose.mock.calls.length === 0)).toBe(true);
    expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
    expect(mocks.terminals[0].reset).toHaveBeenCalled();
  });

  it("为 PTY 托管的 TUI 关闭 convertEol，避免全屏界面叠成两层", async () => {
    renderHost();
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));
    expect(mocks.terminals[0].options).toEqual(expect.objectContaining({
      convertEol: false,
      cursorBlink: false,
      scrollback: 2000,
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

    renderHost();
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

    renderHost();
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));
    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);

    await waitFor(() => {
      expect(screen.getAllByRole("alert")[0]).toHaveTextContent("Error: boom");
    });
    expect(mocks.terminals[0].reset).toHaveBeenCalled();
    expect(mocks.terminals[0].writes.some((line) => line.includes("点击启动后"))).toBe(true);
  });

  it("Claude 与 Codex 终端之间提供可访问的拖动分隔线", async () => {
    renderHost();
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

    renderHost();
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

    const { unmount } = renderHost();
    await waitFor(() => expect(mocks.listen).toHaveBeenCalled());
    expect(mocks.listeners.get("terminal-output") ?? []).toHaveLength(0);
    unmount();
    outputReady.resolve();

    await waitFor(() => {
      expect(mocks.listeners.get("terminal-output") ?? []).toHaveLength(0);
    });
  });

  it("每个 PTY 数据块只写入一次，不会因为重复订阅叠成两层", async () => {
    renderHost();
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));
    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
      expect(mocks.listeners.get("terminal-output")).toHaveLength(1);
    });

    emitEvent("terminal-output", { sessionId: "terminal-claude-1", data: "use one." });
    expect(mocks.terminals[0].writes.filter((chunk) => chunk.includes("use one."))).toEqual(["use one."]);
  });

  it("startWith 命令式启动：携带 resume 参数（在终端中续聊）", async () => {
    const ref = { current: null as TerminalWorkspaceHandle | null };
    render(
      <TerminalWorkspace
        ref={ref as unknown as React.Ref<TerminalWorkspaceHandle>}
        active
        projectWorkDir={PROJECT_DIR}
        onProjectWorkDirChange={() => {}}
      />,
    );
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());

    act(() => ref.current?.startWith("claude", { args: ["--resume", "session-42"] }));
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("start_terminal", {
        request: expect.objectContaining({
          agent: "claude",
          work_dir: PROJECT_DIR,
          args: ["--resume", "session-42"],
        }),
      });
    });
  });

  it("start_terminal 返回前到达的输出不丢（避免 Claude 启动后黑屏）", async () => {
    let releaseStart!: (value: {
      id: string;
      agent: string;
      work_dir: string;
      status: string;
    }) => void;
    const startPromise = new Promise<{
      id: string;
      agent: string;
      work_dir: string;
      status: string;
    }>((resolve) => {
      releaseStart = resolve;
    });
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_terminal") return startPromise;
      return Promise.resolve(undefined);
    });

    renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());
    await waitFor(() => expect(mocks.listeners.get("terminal-output")).toHaveLength(1));

    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    // PTY 已在跑、前端尚未写入 session：早期输出必须进 orphan 缓冲
    emitEvent("terminal-output", { sessionId: "terminal-claude-early", data: "welcome-tui" });
    expect(mocks.terminals[0].writes.some((chunk) => chunk.includes("welcome-tui"))).toBe(false);

    await act(async () => {
      releaseStart({
        id: "terminal-claude-early",
        agent: "claude",
        work_dir: PROJECT_DIR,
        status: "running",
      });
      await startPromise;
    });

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
    });
    await waitFor(() => {
      expect(mocks.terminals[0].writes.some((chunk) => chunk.includes("welcome-tui"))).toBe(true);
    });
  });

  it("start_terminal 返回前 CLI 已退出：面板进入 exited 而非永久 running", async () => {
    const startPromise = deferred<{
      id: string;
      agent: string;
      work_dir: string;
      status: string;
    }>();
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_terminal") return startPromise.promise;
      return Promise.resolve(undefined);
    });

    renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());
    await waitFor(() => expect(mocks.listeners.get("terminal-exit")).toHaveLength(1));

    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    // CLI 在 invoke 返回前就退出（坏二进制秒退：exit code 0，无输出）
    emitEvent("terminal-exit", { sessionId: "terminal-claude-early", code: 0 });

    await act(async () => {
      startPromise.resolve({
        id: "terminal-claude-early",
        agent: "claude",
        work_dir: PROJECT_DIR,
        status: "running",
      });
      await startPromise.promise;
    });

    // 不得卡在"运行中"（无停止按钮）；应回到可再次启动的 exited 状态
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "停止 Claude CLI" })).not.toBeInTheDocument();
    });
    expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled();
  });

  it("start_terminal 返回前 CLI 异常退出：显示退出码而非卡 running", async () => {
    const startPromise = deferred<{
      id: string;
      agent: string;
      work_dir: string;
      status: string;
    }>();
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_terminal") return startPromise.promise;
      return Promise.resolve(undefined);
    });

    renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());
    await waitFor(() => expect(mocks.listeners.get("terminal-exit")).toHaveLength(1));

    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    emitEvent("terminal-exit", { sessionId: "terminal-claude-early", code: 1 });

    await act(async () => {
      startPromise.resolve({
        id: "terminal-claude-early",
        agent: "claude",
        work_dir: PROJECT_DIR,
        status: "running",
      });
      await startPromise.promise;
    });

    await waitFor(() => {
      expect(screen.getAllByRole("alert")[0]).toHaveTextContent("CLI 异常退出（代码 1）");
    });
    expect(screen.queryByRole("button", { name: "停止 Claude CLI" })).not.toBeInTheDocument();
  });

  it("startClaudeForTask 可连续两次启动，不出现「终端已在运行」", async () => {
    let startCount = 0;
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_terminal") {
        startCount += 1;
        return Promise.resolve({
          id: `terminal-claude-${startCount}`,
          agent: "claude",
          work_dir: PROJECT_DIR,
          status: "running",
        });
      }
      return Promise.resolve(undefined);
    });

    const ref = { current: null as TerminalWorkspaceHandle | null };
    render(
      <TerminalWorkspace
        ref={ref as unknown as React.Ref<TerminalWorkspaceHandle>}
        active
        projectWorkDir={PROJECT_DIR}
        onProjectWorkDirChange={() => {}}
      />,
    );
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());

    let firstId = "";
    let secondId = "";
    await act(async () => {
      firstId = await ref.current!.startClaudeForTask(PROJECT_DIR);
    });
    await act(async () => {
      secondId = await ref.current!.startClaudeForTask(PROJECT_DIR);
    });

    expect(firstId).toBe("terminal-claude-1");
    expect(secondId).toBe("terminal-claude-2");
    expect(mocks.invoke.mock.calls.filter((call) => call[0] === "start_terminal")).toHaveLength(2);
    expect(mocks.invoke.mock.calls.filter((call) => call[0] === "stop_terminal")).toHaveLength(1);
    expect(screen.queryByText(/终端已在运行/)).not.toBeInTheDocument();
  });

  it("点击 + 新增一个空闲的 Claude 终端标签", async () => {
    renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());

    // 初始只有一个 Claude 标签
    expect(screen.getAllByRole("tab")).toHaveLength(1);

    await userEvent.click(screen.getByRole("button", { name: "新增 Claude 终端" }));

    // 新增后有两个标签，新标签为空闲态（可再启动）
    expect(screen.getAllByRole("tab")).toHaveLength(2);
    expect(screen.getByRole("tab", { name: /Claude CLI 2/ })).toBeInTheDocument();
  });

  it("agent 启动条：注册表渲染芯片，点击新开对应 Agent 标签", async () => {
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "agent_catalog") {
        return Promise.resolve([
          { id: "claude", name: "Claude Code", can_work: true, can_review: true, installed: true, sessions_present: true },
          { id: "gemini", name: "Gemini CLI", can_work: true, can_review: true, installed: true, sessions_present: false },
          { id: "dsh", name: "DeepSeek DSH", can_work: true, can_review: true, installed: false, sessions_present: false },
        ]);
      }
      if (command === "start_terminal") {
        return Promise.resolve({
          id: "terminal-claude-1",
          agent: "claude",
          work_dir: PROJECT_DIR,
          status: "running",
        });
      }
      return undefined;
    });
    renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());

    // 启动条渲染注册表条目（含未安装的 dsh，弱化展示）
    expect(screen.getByRole("button", { name: /Gemini CLI/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /DeepSeek DSH/ })).toBeInTheDocument();

    // 点击 gemini 芯片 → 新开 Gemini 标签（工人列泛化）
    await userEvent.click(screen.getByRole("button", { name: /Gemini CLI/ }));
    expect(await screen.findByRole("tab", { name: /Gemini CLI 2/ })).toBeInTheDocument();
  });

  it("停止与自行退出竞态：invoke 失败不再回滚 running 卡死面板", async () => {
    const stopRejected = deferred<void>();
    mocks.invoke.mockImplementation((command: string) => {
      if (command === "start_terminal") {
        return Promise.resolve({
          id: "terminal-claude-1",
          agent: "claude",
          work_dir: PROJECT_DIR,
          status: "running",
        });
      }
      if (command === "stop_terminal") {
        return stopRejected.promise.then(() => {
          throw new Error("终端会话不存在或已退出");
        });
      }
      return Promise.resolve(undefined);
    });

    renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());
    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    await waitFor(() => expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument());

    // 点停止（invoke 挂起）期间 CLI 自行退出：exit 事件先把面板置 exited
    await userEvent.click(screen.getByRole("button", { name: "停止 Claude CLI" }));
    emitEvent("terminal-exit", { sessionId: "terminal-claude-1", code: 0 });

    // invoke 现在才失败。旧代码无条件回滚 running，造成"停止按钮无响应、
    // 启动按钮不出现"的永久卡死；修复后保持 exited
    stopRejected.resolve();
    await waitFor(() => {
      expect(screen.getAllByRole("button", { name: "启动" })).toHaveLength(2);
    });
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "停止 Claude CLI" })).not.toBeInTheDocument();
    });
  });

  it("点击文件夹图标打开目录选择器，并只更新对应面板的工作目录", async () => {
    mocks.dialogOpen.mockResolvedValue("D:\\picked-project");
    renderHost();
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

  it("Claude 运行中切到不同目录时提示归属不符，不停止会话", async () => {
    const { rerender } = renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());
    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    await waitFor(() => expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument());

    rerender(<WorkspaceHost projectDir="D:\\other-space" />);
    const banner = await screen.findByRole("status");
    expect(banner).toHaveTextContent("Harness_agent");
    expect(banner).toHaveTextContent("other-space");
    expect(banner).toHaveTextContent("停止后重新启动才会切到新空间");
    expect(mocks.invoke.mock.calls.some((call) => call[0] === "stop_terminal")).toBe(false);
    expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
  });

  it("Claude 运行中目录仍匹配时不提示；未启动时换目录也不提示", async () => {
    const { rerender } = renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })[0]).toBeEnabled());
    rerender(<WorkspaceHost projectDir="D:\\other-space" />);
    expect(screen.queryByRole("status")).not.toBeInTheDocument();

    rerender(<WorkspaceHost projectDir={PROJECT_DIR} />);
    await userEvent.click(screen.getAllByRole("button", { name: "启动" })[0]);
    await waitFor(() => expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument());
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("切空间横幅只出现在 Claude pane，不出现在 Codex pane", async () => {
    mocks.invoke.mockImplementation((command: string, payload?: { request?: { agent: string } }) => {
      if (command === "start_terminal") {
        const agent = payload?.request?.agent ?? "claude";
        return Promise.resolve({
          id: `terminal-${agent}-1`,
          agent,
          work_dir: agent === "claude" ? PROJECT_DIR : PROJECT_DIR,
          status: "running",
        });
      }
      return Promise.resolve(undefined);
    });
    const { rerender } = renderHost();
    await waitFor(() => expect(screen.getAllByRole("button", { name: "启动" })).toHaveLength(2));
    const startButtons = screen.getAllByRole("button", { name: "启动" });
    await userEvent.click(startButtons[0]);
    await userEvent.click(startButtons[1]);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "停止 Claude CLI" })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "停止 Codex CLI" })).toBeInTheDocument();
    });
    rerender(<WorkspaceHost projectDir="D:\\other-space" />);
    const banners = await screen.findAllByRole("status");
    expect(banners).toHaveLength(1);
    expect(banners[0]).toHaveTextContent("Claude");
  });

  it("目录选择器取消时保持当前工作目录不变", async () => {
    mocks.dialogOpen.mockResolvedValue(null);
    renderHost();
    await waitFor(() => expect(mocks.terminals).toHaveLength(2));

    await userEvent.click(screen.getByRole("button", { name: "选择 Codex CLI 工作目录" }));
    expect(screen.getByLabelText("Codex CLI 工作目录")).toHaveValue(
      "F:\\project\\workspace-side\\Harness_agent",
    );
  });
});
