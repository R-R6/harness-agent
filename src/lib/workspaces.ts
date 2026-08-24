// 工作空间模型（学习 Codex 的 Project 一级实体设计，见 docs/workspace-plan.md）：
// 空间是持久化实体（路径为身份），任务/表单/看板都挂在"激活空间"上；
// 换目录 = 切换/新建空间，而不是原地改某个全局字符串。
import type { Dispatch, SetStateAction } from "react";

export interface Workspace {
  id: string;
  /** 绝对目录（身份，建后不改；要换目录就切/建另一个空间） */
  path: string;
  /** 显示名（默认目录 basename，阶段 D 开放编辑） */
  name: string;
  /** 列表保序 */
  position: number;
  createdAt: number;
}

const LIST_KEY = "ha-workspaces";
const ACTIVE_KEY = "ha-active-workspace";
/** 旧版单目录持久化键：非空时一次性迁移为第一个空间 */
const LEGACY_DIR_KEY = "ha-project-work-dir";

function readJson<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : null;
  } catch {
    return null;
  }
}

function writeJson(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // 持久化失败不阻断内存态使用
  }
}

export function basenameOf(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const seg = trimmed.split(/[\\/]/).pop() ?? trimmed;
  return seg || trimmed;
}

function newId(): string {
  return `ws-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

export function makeWorkspace(path: string, position: number): Workspace {
  return {
    id: newId(),
    path,
    name: basenameOf(path),
    position,
    createdAt: Date.now(),
  };
}

/** 读取空间列表 + 激活 id。首次调用做旧版单目录的一次性迁移：
 *  旧值非空 → 成为第一个空间并激活；随后清掉旧键（幂等）。 */
export function initWorkspaces(): { list: Workspace[]; activeId: string | null } {
  const stored = readJson<Workspace[]>(LIST_KEY) ?? [];
  let activeId = localStorage.getItem(ACTIVE_KEY);
  if (stored.length === 0) {
    try {
      const legacy = localStorage.getItem(LEGACY_DIR_KEY)?.trim();
      if (legacy) {
        const first = makeWorkspace(legacy, 0);
        stored.push(first);
        activeId = first.id;
        localStorage.removeItem(LEGACY_DIR_KEY);
      }
    } catch {
      // localStorage 不可用：保持空列表
    }
  }
  // 激活 id 失效时回退第一个
  if (stored.length > 0 && !stored.some((w) => w.id === activeId)) {
    activeId = stored[0].id;
  }
  if (stored.length === 0) activeId = null;
  return { list: stored, activeId };
}

export function saveWorkspaces(list: Workspace[], activeId: string | null) {
  writeJson(LIST_KEY, list);
  if (activeId) {
    try {
      localStorage.setItem(ACTIVE_KEY, activeId);
    } catch {
      /* 同上 */
    }
  }
}

export function nextPosition(list: Workspace[]): number {
  return list.reduce((max, w) => Math.max(max, w.position), -1) + 1;
}

/** Windows 风格的路径等价比较（大小写不敏感 + 分隔符归一 + 去尾分隔符） */
export function samePath(a: string, b: string): boolean {
  const norm = (p: string) => p.replace(/[\\/]+$/, "").replace(/\//g, "\\").toLowerCase();
  return norm(a) === norm(b);
}

/**
 * 目录输入桥接（阶段 A 过渡：旧 UI 仍是"输入目录"，新模型是空间列表）。
 * 语义：输入的目录匹配既有空间 → 激活它（Codex 的"切目录=切空间"心智）；
 * 否则若有激活空间 → 更新其路径（保留 id/位置）；列表为空 → 建第一个空间。
 * 纯函数，便于单测；App 侧 setState + saveWorkspaces。
 */
export function resolveDirChange(
  list: Workspace[],
  activeId: string | null,
  rawDir: string,
): { list: Workspace[]; activeId: string | null; changed: boolean } {
  const dir = rawDir.trim();
  if (dir === "") {
    // 清空输入不销毁空间模型（表单校验会拦空目录）
    return { list, activeId, changed: false };
  }
  const hit = list.find((w) => samePath(w.path, dir));
  if (hit) {
    return { list, activeId: hit.id, changed: hit.id !== activeId };
  }
  if (list.length === 0) {
    const first = makeWorkspace(dir, 0);
    return { list: [first], activeId: first.id, changed: true };
  }
  const active = list.find((w) => w.id === activeId);
  if (!active) {
    // 无激活空间（异常态）：作为新空间追加并激活
    const added = makeWorkspace(dir, nextPosition(list));
    return { list: [...list, added], activeId: added.id, changed: true };
  }
  if (samePath(active.path, dir)) {
    return { list, activeId, changed: false };
  }
  const updated = list.map((w) =>
    w.id === active.id ? { ...w, path: dir, name: basenameOf(dir) } : w,
  );
  return { list: updated, activeId, changed: true };
}

export type WorkspacesSetter = Dispatch<SetStateAction<Workspace[]>>;
