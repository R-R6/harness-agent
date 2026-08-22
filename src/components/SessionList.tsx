import { Fragment, type CSSProperties, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { formatShort } from "../lib/formatTime";
import { exportTranscriptMd } from "../lib/api";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { save } from "@tauri-apps/plugin-dialog";
import type { SessionInfo } from "../types";
import { Icon, type IconName } from "./Icon";
import { SplitHandle } from "./SplitHandle";
import { useElementSize, useStoredNumber } from "../lib/layoutPreferences";

interface Props {
  sessions: SessionInfo[];
  selectedFile: string | null;
  onSelect: (s: SessionInfo) => void;
  /** 是否处于搜索态（空组时提示"无匹配结果"） */
  searching?: boolean;
  /** 搜索结果中按 Esc 恢复完整列表 */
  onEscapeSearch?: () => void;
  /** 「在终端中续聊」：切到终端工作台以对应 CLI resume 该会话 */
  onResumeInTerminal?: (s: SessionInfo) => void;
}

/** 固定双栏顺序：Claude 左 / Codex 右（不依赖数据顺序） */
const AGENT_ORDER: { agent: string; label: string }[] = [
  { agent: "claude", label: "Claude Code" },
  { agent: "codex", label: "Codex" },
];
const MIN_COLUMN_WIDTH = 160;
const MIN_COLUMN_HEIGHT = 120;
const SPLITTER_SIZE = 12;

function clampRatio(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), Math.max(min, max));
}

/** 文件名（无标题时的回退显示） */
function fileName(file: string): string {
  return file.split(/[\\/]/).pop() ?? file;
}

/** 会话 ID：文件名去扩展名（Claude UUID / Codex rollout-xxx） */
export function sessionId(file: string): string {
  const base = fileName(file);
  return base.endsWith(".jsonl") ? base.slice(0, -6) : base;
}

/** 续聊命令（按 agent 生成，可直接粘贴到终端） */
function resumeCommand(s: SessionInfo): string {
  const id = sessionId(s.file);
  return s.agent === "codex" ? `codex resume ${id}` : `claude --resume ${id}`;
}

/** 复制文本到剪贴板（WebView2 的 navigator.clipboard，用户手势下可用） */
async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      // 降级：隐藏 textarea + execCommand（旧 WebView2）
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
      return true;
    } catch {
      return false;
    }
  }
}

interface MenuState {
  x: number;
  y: number;
  session: SessionInfo;
}

/**
 * 会话列表：Claude / Codex 双栏并列（各自独立滚动，中间分割线可拖）
 * 右键菜单：复制路径/ID/续聊命令、在文件夹中显示、收藏置顶、导出 Markdown
 */
export function SessionList({ sessions, selectedFile, onSelect, searching = false, onEscapeSearch, onResumeInTerminal }: Props) {
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [toast, setToast] = useState("");
  const [starred, setStarred] = useState<Set<string>>(() => {
    try {
      const raw = localStorage.getItem("ha-starred");
      return raw ? new Set(JSON.parse(raw) as string[]) : new Set();
    } catch {
      return new Set();
    }
  });
  const toastTimer = useRef<number | null>(null);
  const buttonRefs = useRef(new Map<string, HTMLButtonElement>());

  const columnsRef = useRef<HTMLDivElement>(null);
  const columnsSize = useElementSize(columnsRef);
  const stacked = columnsSize.width > 0 && columnsSize.width < 420;
  const [wideRatio, setWideRatio] = useStoredNumber("ha-layout-session-agent-width", 50);
  const [stackedRatio, setStackedRatio] = useStoredNumber("ha-layout-session-agent-height", 50);
  const axisSize = stacked ? columnsSize.height : columnsSize.width;
  const minimumPaneSize = stacked ? MIN_COLUMN_HEIGHT : MIN_COLUMN_WIDTH;
  const minRatio = axisSize > 0
    ? Math.min(50, Math.max(20, (minimumPaneSize / Math.max(1, axisSize - SPLITTER_SIZE)) * 100))
    : 30;
  const maxRatio = 100 - minRatio;
  const ratio = clampRatio(stacked ? stackedRatio : wideRatio, minRatio, maxRatio);
  const setRatio = stacked ? setStackedRatio : setWideRatio;

  const showToast = useCallback((msg: string) => {
    setToast(msg);
    if (toastTimer.current) window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(""), 1800);
  }, []);

  // 全局点击/右键/滚动/窗口缩放关闭菜单
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
    };
  }, [menu]);

  // 组件卸载时清理 toast 定时器（防泄漏）
  useEffect(() => {
    return () => {
      if (toastTimer.current) window.clearTimeout(toastTimer.current);
    };
  }, []);

  // 收藏切换（localStorage 持久化）
  const toggleStar = useCallback((file: string) => {
    setStarred((prev) => {
      const next = new Set(prev);
      if (next.has(file)) {
        next.delete(file);
      } else {
        next.add(file);
      }
      try {
        localStorage.setItem("ha-starred", JSON.stringify([...next]));
      } catch {
        // 忽略存储失败
      }
      return next;
    });
  }, []);

  // 分组 + 收藏置顶
  const groups = useMemo(() => {
    const map = new Map<string, SessionInfo[]>();
    for (const s of sessions) {
      const arr = map.get(s.agent) ?? [];
      arr.push(s);
      map.set(s.agent, arr);
    }
    for (const list of map.values()) {
      list.sort((a, b) => {
        const sa = starred.has(a.file) ? 0 : 1;
        const sb = starred.has(b.file) ? 0 : 1;
        if (sa !== sb) return sa - sb;
        return b.updated.localeCompare(a.updated);
      });
    }
    return map;
  }, [sessions, starred]);

  const visibleSessions = useMemo(
    () => AGENT_ORDER.flatMap(({ agent }) => groups.get(agent) ?? []),
    [groups],
  );

  const handleItemKeyDown = useCallback((event: React.KeyboardEvent<HTMLButtonElement>, session: SessionInfo) => {
    if (event.key === "Escape" && onEscapeSearch) {
      event.preventDefault();
      onEscapeSearch();
      return;
    }
    const delta = event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
    if (!delta && event.key !== "Home" && event.key !== "End") return;
    event.preventDefault();
    const index = visibleSessions.findIndex((item) => item.file === session.file);
    if (index < 0) return;
    const nextIndex = event.key === "Home"
      ? 0
      : event.key === "End"
        ? visibleSessions.length - 1
        : Math.min(Math.max(index + delta, 0), visibleSessions.length - 1);
    const next = visibleSessions[nextIndex];
    if (!next) return;
    onSelect(next);
    window.requestAnimationFrame(() => buttonRefs.current.get(next.file)?.focus());
  }, [onEscapeSearch, onSelect, visibleSessions]);

  // 右键菜单动作
  const act = useCallback(
    async (action: string, s: SessionInfo) => {
      setMenu(null);
      const label = s.title?.trim() || fileName(s.file);
      switch (action) {
        case "copy-path":
          if (await copyText(s.file)) showToast("已复制文件路径");
          else showToast("复制失败");
          break;
        case "copy-id":
          if (await copyText(sessionId(s.file))) showToast("已复制会话 ID");
          else showToast("复制失败");
          break;
        case "copy-resume":
          if (await copyText(resumeCommand(s))) showToast("已复制续聊命令");
          else showToast("复制失败");
          break;
        case "reveal":
          try {
            await revealItemInDir(s.file);
          } catch (e) {
            showToast(`打开目录失败: ${String(e)}`);
          }
          break;
        case "star":
          toggleStar(s.file);
          showToast(starred.has(s.file) ? "已取消收藏" : "已收藏（置顶）");
          break;
        case "export-md": {
          try {
            const dest = await save({
              title: "导出会话为 Markdown",
              defaultPath: `${label}.md`,
              filters: [{ name: "Markdown", extensions: ["md"] }],
            });
            if (dest) {
              await exportTranscriptMd(s.file, dest);
              showToast(`已导出: ${dest}`);
            }
          } catch (e) {
            showToast(`导出失败: ${String(e)}`);
          }
          break;
        }
        case "resume-terminal":
          onResumeInTerminal?.(s);
          break;
        default:
          break;
      }
    },
    [showToast, toggleStar, starred, onResumeInTerminal],
  );

  const openMenu = (e: React.MouseEvent, s: SessionInfo) => {
    e.preventDefault();
    // 关键：阻止事件冒泡到 window 的原生 contextmenu 关闭监听，
    // 否则菜单刚打开就被同一个右键事件立即关闭（真实 WebView2 必现）
    e.stopPropagation();
    const x = Math.min(e.clientX, window.innerWidth - 220);
    const y = Math.min(e.clientY, window.innerHeight - 320);
    setMenu({ x, y, session: s });
  };

  if (sessions.length === 0) {
    return <div className="empty">{searching ? "无匹配结果" : "暂无会话"}</div>;
  }

  const menuItems: { action: string; label: string; icon: IconName; danger?: boolean }[] = [
    { action: "copy-path", label: "复制文件路径", icon: "copy" },
    { action: "copy-id", label: "复制会话 ID", icon: "clipboard" },
    { action: "copy-resume", label: "复制续聊命令", icon: "terminal" },
    ...(onResumeInTerminal
      ? [{ action: "resume-terminal", label: "在终端中续聊", icon: "play" as IconName }]
      : []),
    { action: "reveal", label: "在文件夹中显示", icon: "folder-open" },
    { action: "star", label: starred.has(menu?.session.file ?? "") ? "取消收藏" : "收藏（置顶）", icon: "star" },
    { action: "export-md", label: "导出为 Markdown", icon: "upload" },
  ];

  return (
    <div
      className={`session-columns ${stacked ? "session-columns--stacked" : ""}`}
      ref={columnsRef}
      style={{ "--session-primary-size": `${ratio}%` } as CSSProperties}
    >
      {AGENT_ORDER.map(({ agent, label }, idx) => {
        const list = groups.get(agent) ?? [];
        return (
          <Fragment key={agent}>
            {idx > 0 && (
              <SplitHandle
                orientation={stacked ? "horizontal" : "vertical"}
                label="调整 Claude 与 Codex 会话区域"
                value={ratio}
                min={minRatio}
                max={maxRatio}
                onChange={setRatio}
                pixelsPerUnit={Math.max(1, axisSize - SPLITTER_SIZE) / 100}
                step={5}
                className="column-resizer"
                valueText={`Claude 会话区域占比 ${Math.round(ratio)}%`}
              />
            )}
            <div
              className={`session-column session-column--${agent}`}
            >
              <h3 className="group-title">
                <span className={`badge badge-${agent}`}>{label}</span>
                <span className="count">{list.length}</span>
              </h3>
              {list.length === 0 ? (
                <div className="column-empty">
                  {searching ? "无匹配结果" : "暂无会话"}
                </div>
              ) : (
                <div className="column-scroll">
                  <ul className="session-list">
                    {list.map((s) => {
                      const itemLabel = s.title?.trim() || fileName(s.file);
                      const isStarred = starred.has(s.file);
                      return (
                        <li
                          key={s.file}
                          data-agent={s.agent}
                          className={s.file === selectedFile ? "selected" : ""}
                          onContextMenu={(e) => openMenu(e, s)}
                        >
                          <button
                            ref={(element) => {
                              if (element) buttonRefs.current.set(s.file, element);
                              else buttonRefs.current.delete(s.file);
                            }}
                            type="button"
                            onClick={() => onSelect(s)}
                            onKeyDown={(event) => handleItemKeyDown(event, s)}
                            title={s.file}
                            aria-label={`打开会话 ${itemLabel}`}
                          >
                            <span className="file">
                              {isStarred && <span className="star" title="已收藏"><Icon name="star" size={11} /></span>}
                              {itemLabel}
                            </span>
                            <span className="time">{formatShort(s.updated)}</span>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </div>
              )}
            </div>
          </Fragment>
        );
      })}

      {/* 右键菜单 */}
      {menu && (
        <div
          className="ctx-menu"
          style={{ left: menu.x, top: menu.y }}
          onClick={(e) => e.stopPropagation()}
        >
          <div className="ctx-head">
            <span className={`badge badge-${menu.session.agent}`}>
              {menu.session.agentLabel}
            </span>
            <span className="ctx-title">{menu.session.title?.trim() || fileName(menu.session.file)}</span>
          </div>
          {menuItems.map((item) => (
            <button
              key={item.action}
              type="button"
              className={item.danger ? "danger" : ""}
              onClick={() => act(item.action, menu.session)}
            >
              <Icon name={item.icon} size={14} />
              {item.label}
            </button>
          ))}
        </div>
      )}

      {/* 轻提示 */}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
