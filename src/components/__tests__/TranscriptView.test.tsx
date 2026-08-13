import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { TranscriptView } from "../TranscriptView";
import type { TranscriptEntry } from "../../types";

const entries: TranscriptEntry[] = [
  { type: "title", text: "写计算器", at: "2026-08-13T10:00:00+08:00" },
  { type: "user", text: "写个计算器" },
  { type: "assistant", text: "好的，我写一个" },
];

describe("TranscriptView", () => {
  it("渲染每条消息文本", () => {
    render(<TranscriptView entries={entries} />);
    expect(screen.getByText("写计算器")).toBeInTheDocument();
    expect(screen.getByText("写个计算器")).toBeInTheDocument();
    expect(screen.getByText("好的，我写一个")).toBeInTheDocument();
  });

  it("空列表显示占位文案", () => {
    render(<TranscriptView entries={[]} />);
    expect(screen.getByText("无内容")).toBeInTheDocument();
  });

  it("带时间戳时按北京时间显示", () => {
    render(<TranscriptView entries={entries} />);
    // formatFull 把 ISO 转成北京时间：2026-08-13T10:00:00+08:00 → 2026-08-13 10:00:00
    expect(screen.getByText("2026-08-13 10:00:00")).toBeInTheDocument();
  });
});
