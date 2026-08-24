import type { Workspace } from "../lib/workspaces";
import { basenameOf } from "../lib/workspaces";
import { Icon, IconButton } from "./Icon";

interface Props {
  workspaces: Workspace[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onAdd: () => void;
  onRemove: (id: string) => void;
}

/** 左侧空间栏（Codex 侧栏心智）：空间列表 + 激活态 + 添加/移除 */
export function WorkspaceSidebar({ workspaces, activeId, onSelect, onAdd, onRemove }: Props) {
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
              <button
                type="button"
                className={`workspace-item ${w.id === activeId ? "is-active" : ""}`}
                onClick={() => onSelect(w.id)}
                title={w.path}
              >
                <Icon name="folder-open" size={14} />
                <span className="workspace-item__name">{w.id === activeId ? w.name : basenameOf(w.path)}</span>
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
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}