import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "../App";
import type { SessionInfo, TranscriptEntry } from "../types";

// mock tauri invoke + event：不依赖真实 Rust 后端
const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  setWindowTheme: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setTheme: mocks.setWindowTheme }),
}));

const sessions: SessionInfo[] = [
  {
    agent: "claude",
    agentLabel: "Claude Code",
    file: "C:\\fake\\.claude\\projects\\p1\\aaa.jsonl",
    updated: "2026-08-13T10:00:00+08:00",
  },
  {
    agent: "codex",
    agentLabel: "Codex",
    file: "C:\\fake\\.codex\\sessions\\2026\\08\\13\\rollout-x.jsonl",
    updated: "2026-08-13T11:00:00+08:00",
  },
];

const transcript: TranscriptEntry[] = [
  { type: "user", text: "写个计算器" },
  { type: "assistant", text: "好的，我写一个" },
];

describe("App 集成", () => {
  beforeEach(() => {
    localStorage.clear();
    mocks.invoke.mockReset();
    mocks.listen.mockReset();
    mocks.setWindowTheme.mockReset();
    mocks.listen.mockResolvedValue(() => {});
    mocks.setWindowTheme.mockResolvedValue(undefined);
    mocks.invoke.mockImplementation((cmd: string) => {
      if (cmd === "list_sessions") return Promise.resolve(sessions);
      if (cmd === "get_transcript") return Promise.resolve(transcript);
      if (cmd === "search_sessions") return Promise.resolve([sessions[0]]);
      if (cmd === "read_review_artifacts") return Promise.resolve([]);
      return Promise.resolve(null);
    });
  });

  it("启动即加载会话列表", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("Claude Code")).toBeInTheDocument();
      expect(screen.getByText("Codex")).toBeInTheDocument();
    });
    expect(mocks.invoke).toHaveBeenCalledWith("list_sessions", { limit: 50 });
  });

  it("应用主题同步到原生窗口标题栏", async () => {
    render(<App />);

    await waitFor(() => expect(mocks.setWindowTheme).toHaveBeenCalledWith("dark"));
    await userEvent.click(screen.getByRole("button", { name: "切换浅色主题" }));
    await waitFor(() => expect(mocks.setWindowTheme).toHaveBeenLastCalledWith("light"));
  });

  it("工作区导航边界可拖动并保存尺寸", async () => {
    render(<App />);
    const splitter = screen.getByRole("separator", { name: "调整工作区导航宽度" });
    const navigation = screen.getByRole("navigation", { name: "工作区导航" });

    fireEvent.pointerDown(splitter, { button: 0, pointerId: 7, clientX: 248 });
    fireEvent.pointerMove(splitter, { pointerId: 7, clientX: 288 });
    fireEvent.pointerUp(splitter, { pointerId: 7 });

    expect(navigation).toHaveStyle({ width: "288px" });
    await waitFor(() => expect(localStorage.getItem("ha-layout-nav-width")).toBe("288"));
  });

  it("点击会话加载正文", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByText("aaa.jsonl")).toBeInTheDocument());
    await userEvent.click(screen.getByText("aaa.jsonl"));
    await waitFor(() => {
      expect(screen.getByText("写个计算器")).toBeInTheDocument();
      expect(screen.getByText("好的，我写一个")).toBeInTheDocument();
    });
    expect(mocks.invoke).toHaveBeenCalledWith("get_transcript", {
      file: sessions[0].file,
      tail: 200,
    });
  });

  it("搜索关键词 → 列表变为搜索结果 + 显示搜索提示条", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByLabelText("搜索关键词")).toBeInTheDocument());
    await userEvent.type(screen.getByLabelText("搜索关键词"), "计算器");
    await userEvent.keyboard("{Enter}");
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("search_sessions", { keyword: "计算器" });
    });
    // 搜索提示条：显示关键词 + 结果数
    expect(screen.getByText(/搜索.*计算器.*条/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "清除搜索" })).toBeInTheDocument();
  });

  it("清除搜索 → 恢复全部会话（重新 list_sessions）", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByLabelText("搜索关键词")).toBeInTheDocument());
    await userEvent.type(screen.getByLabelText("搜索关键词"), "计算器");
    await userEvent.keyboard("{Enter}");
    await waitFor(() => expect(screen.getByRole("button", { name: "清除搜索" })).toBeInTheDocument());
    mocks.invoke.mockClear();
    await userEvent.click(screen.getByRole("button", { name: "清除搜索" }));
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("list_sessions", { limit: 50 });
    });
    expect(screen.queryByText(/搜索.*计算器.*条/)).not.toBeInTheDocument();
  });

  it("切换 tab 后会话列表状态保留（双视图常驻）", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());
    // 切到监督闭环再切回
    await userEvent.click(screen.getByRole("button", { name: "监督闭环" }));
    await userEvent.click(screen.getByRole("button", { name: "会话浏览" }));
    // 会话列表仍在（组件未卸载）
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText("Codex")).toBeInTheDocument();
  });

  it("后端报错时显示错误信息", async () => {
    mocks.invoke.mockRejectedValueOnce(new Error("spawn node 失败"));
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText(/spawn node 失败/)).toBeInTheDocument();
    });
  });

  it("输入框聚焦时 Ctrl+1 不切换 tab（快捷键边界）", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByLabelText("搜索关键词")).toBeInTheDocument());
    // 聚焦搜索输入框
    const input = screen.getByLabelText("搜索关键词");
    input.focus();
    fireEvent.keyDown(window, { key: "1", ctrlKey: true });
    // tab 未切换：监督闭环按钮不是 active，搜索框仍在
    expect(screen.getByLabelText("搜索关键词")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "监督闭环" }).className,
    ).not.toContain("active");
  });

  it("刷新按钮 → 重新加载会话列表", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByRole("button", { name: "刷新会话列表" })).toBeInTheDocument());
    mocks.invoke.mockClear();
    await userEvent.click(screen.getByRole("button", { name: "刷新会话列表" }));
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("list_sessions", { limit: 50 });
    });
  });

  it("切回会话浏览 tab 自动刷新（再次 list_sessions）", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByText("Claude Code")).toBeInTheDocument());
    mocks.invoke.mockClear();
    // 切到监督闭环再切回 → 自动刷新
    await userEvent.click(screen.getByRole("button", { name: "监督闭环" }));
    await userEvent.click(screen.getByRole("button", { name: "会话浏览" }));
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("list_sessions", { limit: 50 });
    });
  });

  it("切回已看会话用缓存（不重复调 get_transcript）", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByText("aaa.jsonl")).toBeInTheDocument());
    await userEvent.click(screen.getByText("aaa.jsonl"));
    await waitFor(() => expect(screen.getByText("写个计算器")).toBeInTheDocument());
    // 切到另一个会话
    await userEvent.click(screen.getByText("rollout-x.jsonl"));
    await waitFor(() => expect(screen.getByText("好的，我写一个")).toBeInTheDocument());
    // 切回 aaa：缓存命中，不再调 get_transcript
    mocks.invoke.mockClear();
    await userEvent.click(screen.getByText("aaa.jsonl"));
    await waitFor(() => expect(screen.getByText("写个计算器")).toBeInTheDocument());
    const calls = mocks.invoke.mock.calls.filter((c) => c[0] === "get_transcript");
    expect(calls.length).toBe(0);
  });

  // 400 条消息的 DOM 重组在机器高负载时超过 5s 默认限时（实测 1s~5.4s 波动），
  // 放宽该用例限时防偶发超时
  it("加载更早 → 往前翻页合并更早消息并更新按钮状态", { timeout: 20000 }, async () => {
    const tailPage: TranscriptEntry[] = Array.from({ length: 200 }, (_, i) => ({
      type: i % 2 ? "assistant" : "user",
      text: `近消息 ${i}`,
    }));
    const olderPage: TranscriptEntry[] = Array.from({ length: 200 }, (_, i) => ({
      type: i % 2 ? "assistant" : "user",
      text: `早消息 ${i}`,
    }));
    mocks.invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => {
      if (cmd === "list_sessions") return Promise.resolve(sessions);
      if (cmd === "get_transcript") {
        return Promise.resolve(args.offset !== undefined ? olderPage : tailPage);
      }
      if (cmd === "search_sessions") return Promise.resolve([sessions[0]]);
      if (cmd === "read_review_artifacts") return Promise.resolve([]);
      return Promise.resolve(null);
    });
    render(<App />);
    await waitFor(() => expect(screen.getByText("aaa.jsonl")).toBeInTheDocument());
    await userEvent.click(screen.getByText("aaa.jsonl"));
    // 初始页 200 条 → 有「加载更早」按钮
    await waitFor(() => expect(screen.getByText("近消息 0")).toBeInTheDocument());
    const btn = screen.getByRole("button", { name: "加载更早的消息" });
    expect(btn).toBeInTheDocument();
    // 点击 → 带 offset=当前已加载条数(200) 往前翻页，更早消息合并到顶部
    await userEvent.click(btn);
    await waitFor(() => expect(screen.getByText("早消息 0")).toBeInTheDocument());
    expect(mocks.invoke).toHaveBeenCalledWith("get_transcript", {
      file: sessions[0].file,
      tail: 200,
      offset: 200,
    });
    const transcriptEl = document.querySelector(".transcript") as HTMLElement;
    const texts = Array.from(transcriptEl.querySelectorAll("pre")).map((el) => el.textContent);
    expect(texts.indexOf("早消息 0")).toBeLessThan(texts.indexOf("近消息 0"));
    // 该页仍有 200 条 → 按钮还在
    expect(screen.getByRole("button", { name: "加载更早的消息" })).toBeInTheDocument();
  });

  it("会话分隔器支持指针和键盘调整，并为正文保留阅读宽度", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByText("aaa.jsonl")).toBeInTheDocument());

    const layout = document.querySelector(".session-layout") as HTMLDivElement;
    Object.defineProperty(layout, "clientWidth", { configurable: true, value: 760 });

    const sidebar = document.querySelector(".session-sidebar") as HTMLElement;
    const splitter = screen.getByRole("separator", { name: "调整会话列表宽度" });

    fireEvent.pointerDown(splitter, { clientX: 320, pointerId: 1 });
    fireEvent.pointerMove(splitter, { clientX: 900, pointerId: 1 });
    fireEvent.pointerUp(splitter, { clientX: 900, pointerId: 1 });

    // 760px layout - 420px reader target - 12px splitter = 328px sidebar.
    expect(sidebar).toHaveStyle({ width: "328px" });
    expect(splitter).toHaveAttribute("aria-valuemin", "260");
    expect(splitter).toHaveAttribute("aria-valuenow", "328");

    fireEvent.keyDown(splitter, { key: "ArrowLeft" });
    expect(sidebar).toHaveStyle({ width: "304px" });

    fireEvent.keyDown(splitter, { key: "Home" });
    expect(sidebar).toHaveStyle({ width: "260px" });
  });
});
