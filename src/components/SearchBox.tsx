import { useState } from "react";
import { Icon } from "./Icon";

interface Props {
  onSearch: (keyword: string) => void;
  onClear: () => void;
}

/** 关键词搜索框：回车/按钮触发搜索，有词时显示清除按钮 */
export function SearchBox({ onSearch, onClear }: Props) {
  const [kw, setKw] = useState("");

  return (
    <form
      className="search-box"
      onSubmit={(e) => {
        e.preventDefault();
        const trimmed = kw.trim();
        if (trimmed) onSearch(trimmed);
      }}
    >
      <input
        value={kw}
        onChange={(e) => setKw(e.currentTarget.value)}
        placeholder="搜索会话关键词..."
        aria-label="搜索关键词"
      />
      <button type="submit">搜索</button>
      {kw.trim() && (
        <button
          type="button"
          className="clear"
          onClick={() => {
            setKw("");
            onClear();
          }}
        >
          <Icon name="close" size={13} /> 清除
        </button>
      )}
    </form>
  );
}
