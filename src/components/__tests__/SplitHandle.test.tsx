import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { SplitHandle } from "../SplitHandle";

describe("SplitHandle", () => {
  it("resizes a vertical boundary with pointer and keyboard input", () => {
    const onChange = vi.fn();
    render(
      <SplitHandle
        orientation="vertical"
        label="调整测试面板宽度"
        value={300}
        min={200}
        max={420}
        onChange={onChange}
      />,
    );

    const handle = screen.getByRole("separator", { name: "调整测试面板宽度" });
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 300 });
    fireEvent.pointerMove(handle, { pointerId: 1, clientX: 470 });
    expect(onChange).toHaveBeenLastCalledWith(420);

    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(onChange).toHaveBeenLastCalledWith(276);
    fireEvent.keyDown(handle, { key: "Home" });
    expect(onChange).toHaveBeenLastCalledWith(200);
  });

  it("resizes a horizontal boundary on the y axis", () => {
    const onChange = vi.fn();
    render(
      <SplitHandle
        orientation="horizontal"
        label="调整测试面板高度"
        value={260}
        min={160}
        max={500}
        onChange={onChange}
      />,
    );

    const handle = screen.getByRole("separator", { name: "调整测试面板高度" });
    expect(handle).toHaveAttribute("aria-orientation", "horizontal");
    fireEvent.pointerDown(handle, { button: 0, pointerId: 2, clientY: 260 });
    fireEvent.pointerMove(handle, { pointerId: 2, clientY: 310 });
    expect(onChange).toHaveBeenLastCalledWith(310);
    fireEvent.keyDown(handle, { key: "ArrowUp" });
    expect(onChange).toHaveBeenLastCalledWith(236);
    fireEvent.keyDown(handle, { key: "End" });
    expect(onChange).toHaveBeenLastCalledWith(500);
  });
});
