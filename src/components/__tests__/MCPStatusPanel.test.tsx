import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MCPStatusPanel } from "../MCPStatusPanel";
import type { McpStatus } from "../../types";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

const healthy: McpStatus = {
  config_path: "C:\\Users\\admin\\.codex\\config.toml",
  items: [
    { name: "[mcp_servers.agent-sessions] 已注册", ok: true, detail: "注册段存在" },
    { name: 'type = "stdio" 已声明', ok: true, detail: "stdio 传输" },
    { name: "server.js 存在", ok: true, detail: "路径存在" },
  ],
  server_js_exists: true,
  host_exe_exists: true,
  handshake_ok: true,
};

const broken: McpStatus = {
  ...healthy,
  items: [
    { name: "[mcp_servers.agent-sessions] 已注册", ok: false, detail: "缺失" },
    { name: 'type = "stdio" 已声明', ok: false, detail: "缺失" },
    { name: "server.js 存在", ok: true, detail: "路径存在" },
  ],
  handshake_ok: false,
};

describe("MCPStatusPanel", () => {
  beforeEach(() => {
    mocks.invoke.mockReset();
  });

  it("健康时显示全部通过 + 不显示修复按钮", async () => {
    mocks.invoke.mockResolvedValue(healthy);
    const onHealthChange = vi.fn();
    render(<MCPStatusPanel onHealthChange={onHealthChange} />);
    await waitFor(() => {
      expect(screen.getByText(/MCP 注册健康（/)).toBeInTheDocument();
    });
    expect(screen.getByText(/已注册/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "一键修复" })).not.toBeInTheDocument();
    expect(onHealthChange).toHaveBeenLastCalledWith("healthy");
  });

  it("异常时显示修复按钮，点击调用 fix_mcp 并重查", async () => {
    let checkCount = 0;
    mocks.invoke.mockImplementation((cmd: string) => {
      if (cmd === "check_mcp") {
        checkCount += 1;
        // 第 1 次 load = broken（显示修复按钮）；fix 后重查 = healthy
        return Promise.resolve(checkCount === 1 ? broken : healthy);
      }
      if (cmd === "fix_mcp") {
        return Promise.resolve({
          ok: true,
          fixed_items: ["补 type = \"stdio\""],
          backup_path: "C:\\Users\\admin\\.codex\\config.toml.bak-1",
          message: "修复完成：补 type = \"stdio\"（需新开 codex 会话生效）",
        });
      }
      return Promise.resolve(null);
    });
    render(<MCPStatusPanel />);
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "一键修复" })).toBeInTheDocument();
    });
    await userEvent.click(screen.getByRole("button", { name: "一键修复" }));
    await waitFor(() => {
      expect(mocks.invoke).toHaveBeenCalledWith("fix_mcp");
    });
    // 修复后重查 + 显示修复结果
    await waitFor(() => {
      expect(screen.getByText(/已修复/)).toBeInTheDocument();
    });
    expect(screen.getByText(/MCP 注册健康（/)).toBeInTheDocument();
  });

  it("握手失败显示在列表", async () => {
    mocks.invoke.mockResolvedValue(broken);
    render(<MCPStatusPanel />);
    await waitFor(() => {
      expect(screen.getByText(/真实握手/)).toBeInTheDocument();
    });
    expect(screen.getByText(/握手失败/)).toBeInTheDocument();
  });
});
