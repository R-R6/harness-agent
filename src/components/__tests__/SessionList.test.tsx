import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SessionList } from "../SessionList";
import type { SessionInfo } from "../../types";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  reveal: vi.fn(),
  save: vi.fn(),
  writeText: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/plugin-opener", () => ({ revealItemInDir: mocks.reveal }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: mocks.save }));

const sessions: SessionInfo[] = [
  {
    agent: "claude",
    agentLabel: "Claude Code",
    file: "C:\\fake\\.claude\\projects\\p1\\aaa.jsonl",
    updated: "2026-08-13T10:00:00+08:00",
  },
  {
    agent: "claude",
    agentLabel: "Claude Code",
    file: "C:\\fake\\.claude\\projects\\p1\\bbb.jsonl",
    title: "排查休眠竞态",
    updated: "2026-08-13T12:00:00+08:00",
  },
  {
    agent: "codex",
    agentLabel: "Codex",
    file: "C:\\fake\\.codex\\sessions\\2026\\08\\13\\rollout-2026-08-13T15-04-04-abc.jsonl",
    updated: "2026-08-13T15:00:00+08:00",
  },
];

function renderList() {
  return render(
    <SessionList sessions={sessions} selectedFile={null} onSelect={() => {}} />,
  );
}

describe("SessionList", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
    mocks.reveal.mockReset();
    mocks.save.mockReset();
    mocks.writeText.mockReset();
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: mocks.writeText },
      configurable: true,
    });
    localStorage.clear();
  });

  afterEach(() => {
    // 清理右键菜单（全局监听）
    document.body.innerHTML = "";
  });

  it("按 agent 分组渲染（组标题 + 会话数）", () => {
    renderList();
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText("Codex")).toBeInTheDocument();
    // 有标题显示标题，无标题回退文件名
    expect(screen.getByText("排查休眠竞态")).toBeInTheDocument();
    expect(screen.getByText("aaa.jsonl")).toBeInTheDocument();
  });

  it("双栏并列：Claude 左栏 / Codex 右栏，各自独立滚动", () => {
    renderList();
    const columns = document.querySelectorAll(".session-column");
    expect(columns.length).toBe(2);
    const claudeCol = columns[0];
    const codexCol = columns[1];
    expect(claudeCol).toHaveClass("session-column--claude");
    expect(codexCol).toHaveClass("session-column--codex");
    // 左栏是 Claude（含 claude 会话），右栏是 Codex
    expect(claudeCol.querySelector(".badge-claude")).toBeTruthy();
    expect(claudeCol.textContent).toContain("排查休眠竞态");
    expect(codexCol.querySelector(".badge-codex")).toBeTruthy();
    expect(codexCol.textContent).toContain("rollout-2026");
    // 各自滚动容器
    expect(claudeCol.querySelector(".column-scroll")).toBeTruthy();
    expect(codexCol.querySelector(".column-scroll")).toBeTruthy();
    // 中间有可拖分割线
    expect(document.querySelector(".column-resizer")).toBeTruthy();
  });

  it("某组无会话时显示空态（普通态/搜索态文案不同）", () => {
    const onlyClaude = sessions.filter((s) => s.agent === "claude");
    render(
      <SessionList sessions={onlyClaude} selectedFile={null} onSelect={() => {}} />,
    );
    // Codex 栏空态
    expect(screen.getByText("暂无会话")).toBeInTheDocument();
  });

  it("搜索态下空组提示无匹配结果", () => {
    const onlyClaude = sessions.filter((s) => s.agent === "claude");
    render(
      <SessionList
        sessions={onlyClaude}
        selectedFile={null}
        onSelect={() => {}}
        searching
      />,
    );
    expect(screen.getByText("无匹配结果")).toBeInTheDocument();
  });

  it("拖动双栏分割线时保留两栏的最小可读宽度", async () => {
    renderList();
    const columns = document.querySelector(".session-columns") as HTMLElement;
    // jsdom 无布局，mock 容器宽度使百分比计算有效
    vi.spyOn(columns, "getBoundingClientRect").mockReturnValue({
      width: 480,
      height: 600,
      left: 0,
      top: 0,
      right: 480,
      bottom: 600,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    fireEvent(window, new Event("resize"));
    await waitFor(() => {
      expect(screen.getByRole("separator", { name: "调整 Claude 与 Codex 会话区域" }))
        .toHaveAttribute("aria-orientation", "vertical");
    });
    const resizer = document.querySelector(".column-resizer") as HTMLElement;
    fireEvent.pointerDown(resizer, { button: 0, pointerId: 1, clientX: 240 });
    fireEvent.pointerMove(resizer, { pointerId: 1, clientX: 40 });
    fireEvent.pointerUp(resizer, { pointerId: 1 });
    expect(Number.parseFloat(columns.style.getPropertyValue("--session-primary-size"))).toBeCloseTo(
      (160 / (480 - 12)) * 100,
      1,
    );
  });

  it("窄侧栏切换为可拖动的横向分隔线", async () => {
    renderList();
    const columns = document.querySelector(".session-columns") as HTMLElement;
    vi.spyOn(columns, "getBoundingClientRect").mockReturnValue({
      width: 360,
      height: 600,
      left: 0,
      top: 0,
      right: 360,
      bottom: 600,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    fireEvent(window, new Event("resize"));

    const resizer = await screen.findByRole("separator", { name: "调整 Claude 与 Codex 会话区域" });
    await waitFor(() => expect(resizer).toHaveAttribute("aria-orientation", "horizontal"));
    expect(columns).toHaveClass("session-columns--stacked");

    fireEvent.pointerDown(resizer, { button: 0, pointerId: 2, clientY: 300 });
    fireEvent.pointerMove(resizer, { pointerId: 2, clientY: 100 });
    fireEvent.pointerUp(resizer, { pointerId: 2 });
    expect(Number.parseFloat(columns.style.getPropertyValue("--session-primary-size"))).toBeCloseTo(
      (120 / (600 - 12)) * 100,
      1,
    );
  });

  it("点击会话触发 onSelect", async () => {
    const onSelect = vi.fn();
    render(
      <SessionList sessions={sessions} selectedFile={null} onSelect={onSelect} />,
    );
    await userEvent.click(screen.getByText("aaa.jsonl"));
    expect(onSelect).toHaveBeenCalledWith(sessions[0]);
  });

  it("方向键在固定双栏顺序中切换会话，Esc 退出搜索", () => {
    const onSelect = vi.fn();
    const onEscapeSearch = vi.fn();
    render(
      <SessionList
        sessions={sessions}
        selectedFile={null}
        onSelect={onSelect}
        searching
        onEscapeSearch={onEscapeSearch}
      />,
    );
    const first = screen.getByRole("button", { name: "打开会话 排查休眠竞态" });
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(onSelect).toHaveBeenCalledWith(sessions[0]);
    fireEvent.keyDown(first, { key: "Escape" });
    expect(onEscapeSearch).toHaveBeenCalledOnce();
  });

  it("右键会话弹出菜单，复制文件路径", async () => {
    renderList();
    fireEvent.contextMenu(screen.getByText("aaa.jsonl"));
    expect(screen.getByText("复制文件路径")).toBeInTheDocument();
    await userEvent.click(screen.getByText("复制文件路径"));
    await waitFor(() => {
      expect(mocks.writeText).toHaveBeenCalledWith(sessions[0].file);
    });
    expect(await screen.findByText(/已复制文件路径/)).toBeInTheDocument();
  });

  it("复制会话 ID（Codex rollout 去扩展名）", async () => {
    renderList();
    fireEvent.contextMenu(screen.getByText(/rollout-2026/));
    await userEvent.click(screen.getByText("复制会话 ID"));
    await waitFor(() => {
      expect(mocks.writeText).toHaveBeenCalledWith("rollout-2026-08-13T15-04-04-abc");
    });
  });

  it("复制续聊命令（按 agent 不同命令）", async () => {
    renderList();
    fireEvent.contextMenu(screen.getByText("aaa.jsonl"));
    await userEvent.click(screen.getByText("复制续聊命令"));
    await waitFor(() => {
      expect(mocks.writeText).toHaveBeenCalledWith("claude --resume aaa");
    });
    fireEvent.contextMenu(screen.getByText(/rollout-2026/));
    await userEvent.click(screen.getByText("复制续聊命令"));
    await waitFor(() => {
      expect(mocks.writeText).toHaveBeenCalledWith(
        "codex resume rollout-2026-08-13T15-04-04-abc",
      );
    });
  });

  it("在文件夹中显示 → revealItemInDir", async () => {
    mocks.reveal.mockResolvedValue(undefined);
    renderList();
    fireEvent.contextMenu(screen.getByText("aaa.jsonl"));
    await userEvent.click(screen.getByText("在文件夹中显示"));
    await waitFor(() => {
      expect(mocks.reveal).toHaveBeenCalledWith(sessions[0].file);
    });
  });

  it("导出 Markdown → save 对话框 + export_transcript_md", async () => {
    mocks.save.mockResolvedValue("D:\\out\\aaa.md");
    mocks.invoke.mockResolvedValue("D:\\out\\aaa.md");
    renderList();
    fireEvent.contextMenu(screen.getByText("aaa.jsonl"));
    await userEvent.click(screen.getByText("导出为 Markdown"));
    await waitFor(() => {
      expect(mocks.save).toHaveBeenCalled();
      expect(mocks.invoke).toHaveBeenCalledWith("export_transcript_md", {
        file: sessions[0].file,
        dest: "D:\\out\\aaa.md",
      });
    });
  });

  it("收藏后置顶 + 星标标记 + localStorage 持久化", async () => {
    renderList();
    // 收藏 codex 会话
    fireEvent.contextMenu(screen.getByText(/rollout-2026/));
    await userEvent.click(screen.getByText("收藏（置顶）"));
    // 星标出现在列表
    await waitFor(() => {
      expect(screen.getByTitle("已收藏")).toBeInTheDocument();
    });
    // localStorage 持久化
    const saved = JSON.parse(localStorage.getItem("ha-starred") ?? "[]");
    expect(saved).toContain(sessions[2].file);
    // 收藏项在组内排第一
    const listItems = document.querySelectorAll("ul.session-list li");
    const codexGroup = Array.from(listItems).filter((li) =>
      li.getAttribute("data-agent") === "codex",
    );
    expect(codexGroup[0].textContent).toContain("rollout-2026");
  });

  it("点击空白处关闭菜单", async () => {
    renderList();
    fireEvent.contextMenu(screen.getByText("aaa.jsonl"));
    expect(screen.getByText("复制文件路径")).toBeInTheDocument();
    fireEvent.click(document.body);
    await waitFor(() => {
      expect(screen.queryByText("复制文件路径")).not.toBeInTheDocument();
    });
  });
});
