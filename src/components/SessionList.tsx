import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { formatShort } from "../lib/formatTime";
import { exportTranscriptMd } from "../lib/api";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { save } from "@tauri-apps/plugin-dialog";
import type { SessionInfo } from "../types";

interface Props {
  sessions: SessionInfo[];
  selectedFile: string | null;
  onSelect: (s: SessionInfo) => void;
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
 * 会话列表：按 agent 分组 + 右键菜单
 * （复制路径/ID/标题/时间、在文件夹中显示、复制续聊命令、导出 Markdown、收藏）
 */
export function SessionList({ sessions, selectedFile, onSelect }: Props) {
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

  const showToast = useCallback((msg: string) => {
    setToast(msg);
    if (toastTimer.current) window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(""), 1800);
  }, []);

  // 全局点击/右键/滚动关闭菜单
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [menu]);

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
    return Array.from(map.entries());
  }, [sessions, starred]);

  // 右键菜单动作
  const act = useCallback(
    async (action: string, s: SessionInfo) => {
      setMenu(null);
      const label = s.title?.trim() || fileName(s.file);
      switch (action) {
        case "copy-path":
          if (await copyText(s.file)) showToast("✅ 已复制文件路径");
          else showToast("❌ 复制失败");
          break;
        case "copy-id":
          if (await copyText(sessionId(s.file))) showToast("✅ 已复制会话 ID");
          else showToast("❌ 复制失败");
          break;
        case "copy-title":
          if (await copyText(label)) showToast("✅ 已复制会话标题");
          else showToast("❌ 复制失败");
          break;
        case "copy-time":
          if (await copyText(s.updated)) showToast("✅ 已复制发起时间");
          else showToast("❌ 复制失败");
          break;
        case "copy-resume":
          if (await copyText(resumeCommand(s))) showToast("✅ 已复制续聊命令");
          else showToast("❌ 复制失败");
          break;
        case "reveal":
          try {
            await revealItemInDir(s.file);
          } catch (e) {
            showToast(`❌ 打开目录失败: ${String(e)}`);
          }
          break;
        case "star":
          toggleStar(s.file);
          showToast(starred.has(s.file) ? "⭐ 已取消收藏" : "⭐ 已收藏（置顶）");
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
              showToast(`✅ 已导出: ${dest}`);
            }
          } catch (e) {
            showToast(`❌ 导出失败: ${String(e)}`);
          }
          break;
        }
        default:
          break;
      }
    },
    [showToast, toggleStar, starred],
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
    return <div className="empty">暂无会话</div>;
  }

  const menuItems: { action: string; label: string; danger?: boolean }[] = [
    { action: "copy-path", label: "📋 复制文件路径" },
    { action: "copy-id", label: "🆔 复制会话 ID" },
    { action: "copy-title", label: "📝 复制会话标题" },
    { action: "copy-time", label: "⏱️ 复制发起时间" },
    { action: "copy-resume", label: "💻 复制续聊命令" },
    { action: "reveal", label: "📁 在文件夹中显示" },
    { action: "star", label: starred.has(menu?.session.file ?? "") ? "⭐ 取消收藏" : "⭐ 收藏（置顶）" },
    { action: "export-md", label: "📤 导出为 Markdown" },
  ];

  return (
    <div className="session-groups">
      {groups.map(([agent, list]) => (
        <div key={agent} className="session-group">
          <h3 className="group-title">
            <span className={`badge badge-${agent}`}>{list[0].agentLabel}</span>
            <span className="count">{list.length}</span>
          </h3>
          <ul className="session-list">
            {list.map((s) => {
              const label = s.title?.trim() || fileName(s.file);
              const isStarred = starred.has(s.file);
              return (
                <li
                  key={s.file}
                  data-agent={s.agent}
                  className={s.file === selectedFile ? "selected" : ""}
                  onContextMenu={(e) => openMenu(e, s)}
                >
                  <button type="button" onClick={() => onSelect(s)} title={s.file}>
                    <span className="file">
                      {isStarred && <span className="star">⭐</span>}
                      {label}
                    </span>
                    <span className="time">{formatShort(s.updated)}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      ))}

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
