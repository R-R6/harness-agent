import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ReviewBoard } from "../ReviewBoard";
import type { ReviewArtifact } from "../../types";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

const artifacts: ReviewArtifact[] = [
  {
    round: 1,
    verdict: "REVIEW",
    reason: "缺少输入校验，请补充。",
    model: "gpt-5.6-luna",
    session_id: "mock-0001",
    file: "C:\\Users\\u\\.claude\\projects\\p\\session-abc.jsonl",
  },
  {
    round: 2,
    verdict: "PASS",
    reason: "已补充输入校验，测试通过。",
    model: "gpt-5.6-luna",
    session_id: "mock-0001",
  },
];

describe("ReviewBoard", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
  });

  it("加载并渲染逐轮审查卡片（verdict/reason/model）", async () => {
    mocks.invoke.mockResolvedValue(artifacts);
    render(<ReviewBoard workDir={"D:\\work"} />);
    await waitFor(() => {
      expect(screen.getByText("第 1 轮")).toBeInTheDocument();
      expect(screen.getByText("第 2 轮")).toBeInTheDocument();
      expect(screen.getByText("缺少输入校验，请补充。")).toBeInTheDocument();
      expect(screen.getByText("已补充输入校验，测试通过。")).toBeInTheDocument();
    });
    expect(mocks.invoke).toHaveBeenCalledWith("read_review_artifacts", {
      workDir: "D:\\work",
    });
  });

  it("PASS 卡片显示通过，REVIEW 卡片显示需返工", async () => {
    mocks.invoke.mockResolvedValue(artifacts);
    render(<ReviewBoard workDir={"D:\\work"} />);
    await waitFor(() => {
      expect(screen.getByText("通过")).toBeInTheDocument();
      expect(screen.getByText("需返工")).toBeInTheDocument();
    });
  });

  it("空产物显示占位文案", async () => {
    mocks.invoke.mockResolvedValue([]);
    render(<ReviewBoard workDir={"D:\\work"} />);
    await waitFor(() => {
      expect(screen.getByText(/暂无审查记录/)).toBeInTheDocument();
    });
  });

  it("后端报错显示错误", async () => {
    mocks.invoke.mockRejectedValueOnce(new Error("读 .supervise 失败"));
    render(<ReviewBoard workDir={"D:\\work"} />);
    await waitFor(() => {
      expect(screen.getByText(/读 \.supervise 失败/)).toBeInTheDocument();
    });
  });

  it("带 file 的轮次显示「查看会话」，点击回调完整路径", async () => {
    mocks.invoke.mockResolvedValue(artifacts);
    const onViewSession = vi.fn();
    render(<ReviewBoard workDir={"D:\\work"} onViewSession={onViewSession} />);
    await waitFor(() => expect(screen.getByText("第 1 轮")).toBeInTheDocument());
    // 只有第 1 轮（带 file）有按钮，第 2 轮（无 file）没有
    const buttons = screen.getAllByRole("button", { name: "查看会话" });
    expect(buttons).toHaveLength(1);
    await userEvent.click(buttons[0]);
    expect(onViewSession).toHaveBeenCalledWith(
      "C:\\Users\\u\\.claude\\projects\\p\\session-abc.jsonl",
    );
  });

  it("未传 onViewSession 时不显示跳转按钮", async () => {
    mocks.invoke.mockResolvedValue(artifacts);
    render(<ReviewBoard workDir={"D:\\work"} />);
    await waitFor(() => expect(screen.getByText("第 1 轮")).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "查看会话" })).not.toBeInTheDocument();
  });

  it("切入监督 tab（active 翻 true）自动刷新，不再需要手点刷新", async () => {
    mocks.invoke.mockResolvedValue(artifacts);
    const { rerender } = render(<ReviewBoard workDir={"D:\\work"} active={false} />);
    // 未激活时不加载（tab 常驻但隐藏，保持懒加载）
    expect(mocks.invoke).not.toHaveBeenCalled();

    rerender(<ReviewBoard workDir={"D:\\work"} active={true} />);
    await waitFor(() => expect(mocks.invoke).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.getByText("第 1 轮")).toBeInTheDocument());

    // 失活再切回 → 再刷一次（拿最新落盘的轮次卡片）
    rerender(<ReviewBoard workDir={"D:\\work"} active={false} />);
    rerender(<ReviewBoard workDir={"D:\\work"} active={true} />);
    await waitFor(() => expect(mocks.invoke).toHaveBeenCalledTimes(2));
  });

  it("传入 taskId 时请求带上该参数；省略时 payload 不含 taskId", async () => {
    mocks.invoke.mockResolvedValue([]);
    const { rerender } = render(<ReviewBoard workDir={"D:\\work"} taskId="task-9" />);
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("read_review_artifacts", {
        workDir: "D:\\work",
        taskId: "task-9",
      });
    });
    mocks.invoke.mockClear();
    rerender(<ReviewBoard workDir={"D:\\work"} />);
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("read_review_artifacts", {
        workDir: "D:\\work",
      });
    });
  });
});
