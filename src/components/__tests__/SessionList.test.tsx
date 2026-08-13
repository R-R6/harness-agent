import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SessionList } from "../SessionList";
import type { SessionInfo } from "../../types";

const sessions: SessionInfo[] = [
  {
    agent: "claude",
    agentLabel: "Claude Code",
    file: "C:\\Users\\admin\\.claude\\projects\\p1\\aaa.jsonl",
    updated: "2026-08-13T10:00:00+08:00",
  },
  {
    agent: "codex",
    agentLabel: "Codex",
    file: "C:\\Users\\admin\\.codex\\sessions\\2026\\08\\13\\rollout-x.jsonl",
    updated: "2026-08-13T11:00:00+08:00",
  },
  {
    agent: "claude",
    agentLabel: "Claude Code",
    file: "C:\\Users\\admin\\.claude\\projects\\p2\\bbb.jsonl",
    updated: "2026-08-13T09:00:00+08:00",
  },
];

describe("SessionList", () => {
  it("按 agent 分组渲染（组标题 + 文件名）", () => {
    render(<SessionList sessions={sessions} selectedFile={null} onSelect={() => {}} />);
    // 组标题
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText("Codex")).toBeInTheDocument();
    // 文件名
    expect(screen.getByText("aaa.jsonl")).toBeInTheDocument();
    expect(screen.getByText("bbb.jsonl")).toBeInTheDocument();
    expect(screen.getByText("rollout-x.jsonl")).toBeInTheDocument();
  });

  it("组标题带会话数量", () => {
    render(<SessionList sessions={sessions} selectedFile={null} onSelect={() => {}} />);
    expect(screen.getAllByText("2").length).toBeGreaterThanOrEqual(1); // claude 组 2 个
  });

  it("claude 会话归入 claude 组（组内包含）", () => {
    const { container } = render(
      <SessionList sessions={sessions} selectedFile={null} onSelect={() => {}} />,
    );
    const groups = container.querySelectorAll(".session-group");
    expect(groups).toHaveLength(2);
    const claudeGroup = groups[0];
    expect(claudeGroup.textContent).toContain("Claude Code");
    expect(claudeGroup.textContent).toContain("aaa.jsonl");
    expect(claudeGroup.textContent).toContain("bbb.jsonl");
    expect(claudeGroup.textContent).not.toContain("rollout-x.jsonl");
    const codexGroup = groups[1];
    expect(codexGroup.textContent).toContain("Codex");
    expect(codexGroup.textContent).toContain("rollout-x.jsonl");
  });

  it("空列表显示占位文案", () => {
    render(<SessionList sessions={[]} selectedFile={null} onSelect={() => {}} />);
    expect(screen.getByText("暂无会话")).toBeInTheDocument();
  });

  it("点击会话触发 onSelect（带完整对象）", async () => {
    const onSelect = vi.fn();
    render(<SessionList sessions={sessions} selectedFile={null} onSelect={onSelect} />);
    await userEvent.click(screen.getByText("aaa.jsonl"));
    expect(onSelect).toHaveBeenCalledWith(sessions[0]);
  });

  it("选中项高亮（selected class + data-agent）", () => {
    const { container } = render(
      <SessionList sessions={sessions} selectedFile={sessions[1].file} onSelect={() => {}} />,
    );
    const items = container.querySelectorAll("li[data-agent]");
    // 分组后顺序：claude(aaa, bbb) → codex(rollout-x)；选中的是 codex → items[2]
    expect(items[0].className).not.toContain("selected");
    expect(items[2].className).toContain("selected");
    expect(items[2].getAttribute("data-agent")).toBe("codex");
  });
});
