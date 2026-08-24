import { useState, useRef, useEffect } from "react";
import type { Workspace } from "../lib/workspaces";
import { basenameOf } from "../lib/workspaces";
import { Icon, IconButton } from "./Icon";

interface Props {
  workspaces: Workspace[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onAdd: () => void;
  onRemove: (id: string) => void;
  /** 重命名空间（阶段 D：name 可编辑，默认目录 basename） */
  onRename: (id: string, name: string) => void;
}

/** 左侧空间栏（Codex 侧栏心智）：空间列表 + 激活态 + 添加/移除 + 重命名 */
export function WorkspaceSidebar({ workspaces, activeId, onSelect, onAdd, onRemove, onRename }: Props) {
  return (
    <aside className="workspace-sidebar" aria-label="工作空间">
      <div className="workspace-sidebar__head">
        <h3>工作空间</h3>
        <IconButton label="添加工作空间" onClick={onAdd}>
          <Icon name="plus" size={14} />
        </IconButton>
      </div>
      {workspaces.length === 0 ? (
        <div className="workspace-sidebar__empty">
          <span>点击 + 添加项目目录</span>
        </div>
      ) : (
        <ul className="workspace-sidebar__list">
          {workspaces.map((w) => (
            <li key={w.id}>
              {w.id === activeId ? (
                <ActiveWorkspaceItem
                  workspace={w}
                  onSelect={onSelect}
                  onRemove={onRemove}
                  onRename={onRename}
                />
              ) : (
                <button
                  type="button"
                  className="workspace-item"
                  onClick={() => onSelect(w.id)}
                  title={w.path}
                >
                  <Icon name="folder-open" size={14} />
                  <span className="workspace-item__name">{basenameOf(w.path)}</span>
                  <span
                    className="workspace-item__remove"
                    role="button"
                    title="移除空间"
                    onClick={(e) => {
                      e.stopPropagation();
                      onRemove(w.id);
                    }}
                  >
                    <Icon name="x" size={12} />
                  </span>
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}

/** 激活空间项：支持点按选中 + 双击重命名 */
function ActiveWorkspaceItem({
  workspace,
  onSelect,
  onRemove,
  onRename,
}: {
  workspace: Workspace;
  onSelect: (id: string) => void;
  onRemove: (id: string) => void;
  onRename: (id: string, name: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [editValue, setEditValue] = useState(workspace.name);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (editing) inputRef.current?.focus();
  }, [editing]);

  useEffect(() => {
    setEditValue(workspace.name);
  }, [workspace.name]);

  const commit = () => {
    const trimmed = editValue.trim();
    if (trimmed && trimmed !== workspace.name) {
      onRename(workspace.id, trimmed);
    }
    setEditing(false);
  };

  return (
    <button
      type="button"
      className="workspace-item is-active"
      onClick={() => onSelect(workspace.id)}
      title={workspace.path}
    >
      <Icon name="folder-open" size={14} />
      {editing ? (
        <input
          ref={inputRef}
          className="workspace-item__rename-input"
          value={editValue}
          onChange={(e) => setEditValue(e.currentTarget.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") { setEditing(false); setEditValue(workspace.name); }
          }}
          onClick={(e) => e.stopPropagation()}
        />
      ) : (
        <span
          className="workspace-item__name"
          onDoubleClick={(e) => {
            e.stopPropagation();
            setEditValue(workspace.name);
            setEditing(true);
          }}
        >
          {workspace.name}
        </span>
      )}
      <span
        className="workspace-item__remove"
        role="button"
        title="移除空间"
        onClick={(e) => {
          e.stopPropagation();
          onRemove(workspace.id);
        }}
      >
        <Icon name="x" size={12} />
      </span>
    </button>
  );
}