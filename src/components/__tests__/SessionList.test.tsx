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
];

describe("SessionList", () => {
  it("渲染全部会话（agent 徽章 + 文件名）", () => {
    render(<SessionList sessions={sessions} selectedFile={null} onSelect={() => {}} />);
    expect(screen.getByText("Claude Code")).toBeInTheDocument();
    expect(screen.getByText("Codex")).toBeInTheDocument();
    expect(screen.getByText("aaa.jsonl")).toBeInTheDocument();
    expect(screen.getByText("rollout-x.jsonl")).toBeInTheDocument();
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

  it("选中项高亮（selected class）", () => {
    const { container } = render(
      <SessionList sessions={sessions} selectedFile={sessions[1].file} onSelect={() => {}} />,
    );
    const items = container.querySelectorAll("li");
    expect(items[1].className).toContain("selected");
    expect(items[0].className).not.toContain("selected");
  });
});
