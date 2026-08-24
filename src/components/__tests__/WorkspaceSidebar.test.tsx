import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { WorkspaceSidebar } from "../WorkspaceSidebar";
import { makeWorkspace } from "../../lib/workspaces";

const ws1 = makeWorkspace("D:\\project-alpha", 0);
const ws2 = makeWorkspace("C:\\work\\beta", 1);

function renderSidebar(overrides: Record<string, unknown> = {}) {
  const props = {
    workspaces: [ws1, ws2],
    activeId: ws1.id,
    onSelect: vi.fn(),
    onAdd: vi.fn(),
    onRemove: vi.fn(),
    onRename: vi.fn(),
    ...overrides,
  };
  render(<WorkspaceSidebar {...(props as unknown as React.ComponentProps<typeof WorkspaceSidebar>)} />);
  return {
    onSelect: props.onSelect as ReturnType<typeof vi.fn>,
    onAdd: props.onAdd as ReturnType<typeof vi.fn>,
    onRemove: props.onRemove as ReturnType<typeof vi.fn>,
    onRename: props.onRename as ReturnType<typeof vi.fn>,
  };
}

describe("WorkspaceSidebar", () => {
  it("空列表显示空态提示", () => {
    renderSidebar({ workspaces: [], activeId: null });
    expect(screen.getByText(/点击 \+ 添加项目目录/)).toBeInTheDocument();
  });

  it("渲染空间列表，当前激活高亮", () => {
    renderSidebar();
    const items = screen.getAllByRole("button");
    const activeBtn = items.find((b) => b.className.includes("is-active"));
    expect(activeBtn).toBeDefined();
    expect(activeBtn?.textContent).toContain(ws1.name);
  });

  it("点击空间项调用 onSelect", async () => {
    const props = renderSidebar();
    const buttons = screen.getAllByRole("button");
    const ws2Btn = buttons.find((b) => b.textContent?.includes("beta"));
    expect(ws2Btn).toBeDefined();
    await userEvent.click(ws2Btn!);
    expect(props.onSelect).toHaveBeenCalledWith(ws2.id);
  });

  it("点击移除图标调用 onRemove（不触发 onSelect）", async () => {
    const props = renderSidebar();
    // 两个空间都有移除按钮，取第一个（激活空间的）
    const removeBtns = screen.getAllByTitle("移除空间");
    await userEvent.click(removeBtns[0]);
    expect(props.onRemove).toHaveBeenCalledWith(ws1.id);
    expect(props.onSelect).not.toHaveBeenCalled();
  });

  it("点击添加按钮调用 onAdd", async () => {
    const props = renderSidebar();
    const addBtn = screen.getByRole("button", { name: "添加工作空间" });
    await userEvent.click(addBtn);
    expect(props.onAdd).toHaveBeenCalledOnce();
  });

  it("双击激活空间名称进入编辑模式，回车提交重命名", async () => {
    const props = renderSidebar();
    await userEvent.dblClick(screen.getByText(ws1.name));
    const input = screen.getByRole("textbox") as HTMLInputElement;
    expect(input).toBeInTheDocument();
    await userEvent.clear(input);
    await userEvent.type(input, "新项目");
    await userEvent.keyboard("{Enter}");
    expect(props.onRename).toHaveBeenCalledWith(ws1.id, "新项目");
  });

  it("按 Escape 取消重命名，不改名", async () => {
    const props = renderSidebar();
    await userEvent.dblClick(screen.getByText(ws1.name));
    const input = screen.getByRole("textbox") as HTMLInputElement;
    await userEvent.clear(input);
    await userEvent.type(input, "临时改名");
    await userEvent.keyboard("{Escape}");
    expect(props.onRename).not.toHaveBeenCalled();
    expect(screen.getByText(ws1.name)).toBeInTheDocument();
  });
});