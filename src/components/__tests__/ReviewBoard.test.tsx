import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
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

  it("PASS 卡片显示 ✅ 通过，REVIEW 卡片显示 🔄 需返工", async () => {
    mocks.invoke.mockResolvedValue(artifacts);
    render(<ReviewBoard workDir={"D:\\work"} />);
    await waitFor(() => {
      expect(screen.getByText("✅ 通过")).toBeInTheDocument();
      expect(screen.getByText("🔄 需返工")).toBeInTheDocument();
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
      expect(screen.getByText(/读 .supervise 失败/)).toBeInTheDocument();
    });
  });
});
