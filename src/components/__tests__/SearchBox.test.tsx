import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SearchBox } from "../SearchBox";

describe("SearchBox", () => {
  it("输入关键词回车触发 onSearch（去除首尾空格）", async () => {
    const onSearch = vi.fn();
    render(<SearchBox onSearch={onSearch} onClear={() => {}} />);
    const input = screen.getByLabelText("搜索关键词");
    await userEvent.type(input, "  登录超时  ");
    await userEvent.keyboard("{Enter}");
    expect(onSearch).toHaveBeenCalledWith("登录超时");
  });

  it("空输入回车不触发 onSearch", async () => {
    const onSearch = vi.fn();
    render(<SearchBox onSearch={onSearch} onClear={() => {}} />);
    await userEvent.keyboard("{Enter}");
    expect(onSearch).not.toHaveBeenCalled();
  });

  it("有词时显示清除按钮，点击清空并触发 onClear", async () => {
    const onClear = vi.fn();
    render(<SearchBox onSearch={() => {}} onClear={onClear} />);
    const input = screen.getByLabelText("搜索关键词");
    await userEvent.type(input, "abc");
    const clearBtn = screen.getByRole("button", { name: "清除" });
    await userEvent.click(clearBtn);
    expect(onClear).toHaveBeenCalled();
    expect(input).toHaveValue("");
  });
});
