import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { WorkspaceSidebar } from "../WorkspaceSidebar";
import { makeWorkspace } from "../../lib/workspaces";

const ws1 = makeWorkspace("D:\\project-alpha", 0);
const ws2 = makeWorkspace("C:\\work\\beta", 1);

describe("WorkspaceSidebar", () => {
  it("空列表显示空态提示", () => {
    render(
      <WorkspaceSidebar workspaces={[]} activeId={null} onSelect={vi.fn()} onAdd={vi.fn()} onRemove={vi.fn()} />,
    );
    expect(screen.getByText(/点击 \+ 添加项目目录/)).toBeInTheDocument();
  });

  it("渲染空间列表，当前激活高亮", () => {
    render(
      <WorkspaceSidebar
        workspaces={[ws1, ws2]}
        activeId={ws1.id}
        onSelect={vi.fn()}
        onAdd={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    // ws1 是激活的，显示其 name
    const items = screen.getAllByRole("button");
    const activeBtn = items.find((b) => b.className.includes("is-active"));
    expect(activeBtn).toBeDefined();
    expect(activeBtn?.textContent).toContain(ws1.name);
  });

  it("点击空间项调用 onSelect", async () => {
    const onSelect = vi.fn();
    render(
      <WorkspaceSidebar
        workspaces={[ws1, ws2]}
        activeId={ws1.id}
        onSelect={onSelect}
        onAdd={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    // ws2 按钮
    const buttons = screen.getAllByRole("button");
    const ws2Btn = buttons.find((b) => b.textContent?.includes("beta"));
    expect(ws2Btn).toBeDefined();
    await userEvent.click(ws2Btn!);
    expect(onSelect).toHaveBeenCalledWith(ws2.id);
  });

  it("点击移除图标调用 onRemove（不触发 onSelect）", async () => {
    const onRemove = vi.fn();
    const onSelect = vi.fn();
    render(
      <WorkspaceSidebar
        workspaces={[ws1]}
        activeId={ws1.id}
        onSelect={onSelect}
        onAdd={vi.fn()}
        onRemove={onRemove}
      />,
    );
    // 移除图标是 aria-label="移除空间" 的 span
    const removeBtn = screen.getByTitle("移除空间");
    await userEvent.click(removeBtn);
    expect(onRemove).toHaveBeenCalledWith(ws1.id);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("点击添加按钮调用 onAdd", async () => {
    const onAdd = vi.fn();
    render(
      <WorkspaceSidebar
        workspaces={[ws1]}
        activeId={ws1.id}
        onSelect={vi.fn()}
        onAdd={onAdd}
        onRemove={vi.fn()}
      />,
    );
    const addBtn = screen.getByRole("button", { name: "添加工作空间" });
    await userEvent.click(addBtn);
    expect(onAdd).toHaveBeenCalledOnce();
  });
});