import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { StatusBar } from "../StatusBar";

describe("StatusBar", () => {
  it("无运行任务时不显示 terminal/supervising 芯片", () => {
    render(
      <StatusBar workspace="会话浏览" claudeCount={3} codexCount={2} mcpHealth="healthy" />,
    );
    expect(screen.queryByText(/terminal/)).not.toBeInTheDocument();
    expect(screen.queryByText(/supervising/)).not.toBeInTheDocument();
    expect(screen.getByText("会话浏览")).toBeInTheDocument();
    expect(screen.getByText("MCP 健康")).toBeInTheDocument();
  });

  it("监督任务运行中显示 supervising 芯片", () => {
    render(
      <StatusBar workspace="监督闭环" claudeCount={0} codexCount={0} superviseRunning={1} />,
    );
    expect(screen.getByText("1 supervising")).toBeInTheDocument();
  });

  it("终端与监督同时运行时两个芯片并存", () => {
    render(
      <StatusBar workspace="终端工作台" claudeCount={0} codexCount={0} terminalRunning={2} superviseRunning={1} />,
    );
    expect(screen.getByText("2 terminals")).toBeInTheDocument();
    expect(screen.getByText("1 supervising")).toBeInTheDocument();
  });
});
