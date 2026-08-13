import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "../App";
import type { SessionInfo, TranscriptEntry } from "../types";

// mock tauri invoke：不依赖真实 Rust 后端
const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
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
    mocks.invoke.mockReset();
    mocks.invoke.mockImplementation((cmd: string) => {
      if (cmd === "list_sessions") return Promise.resolve(sessions);
      if (cmd === "get_transcript") return Promise.resolve(transcript);
      if (cmd === "search_sessions") return Promise.resolve([sessions[0]]);
      return Promise.resolve(null);
    });
  });

  it("启动即加载会话列表", async () => {
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText("Claude Code")).toBeInTheDocument();
      expect(screen.getByText("Codex")).toBeInTheDocument();
    });
    expect(mocks.invoke).toHaveBeenCalledWith("list_sessions", {});
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
      tail: undefined,
    });
  });

  it("搜索关键词 → 列表变为搜索结果", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByLabelText("搜索关键词")).toBeInTheDocument());
    await userEvent.type(screen.getByLabelText("搜索关键词"), "计算器");
    await userEvent.keyboard("{Enter}");
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("search_sessions", { keyword: "计算器" });
    });
  });

  it("后端报错时显示错误信息", async () => {
    mocks.invoke.mockRejectedValueOnce(new Error("spawn node 失败"));
    render(<App />);
    await waitFor(() => {
      expect(screen.getByText(/spawn node 失败/)).toBeInTheDocument();
    });
  });
});
