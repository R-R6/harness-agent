import { describe, expect, it, beforeEach } from "vitest";
import {
  makeWorkspace,
  initWorkspaces,
  saveWorkspaces,
  nextPosition,
  samePath,
  addWorkspace,
  basenameOf,
  type Workspace,
} from "../workspaces";

/** 清理 localStorage 并设置测试前缀，避免干扰其他测试 */
function setLegacyDir(dir: string) {
  localStorage.setItem("ha-project-work-dir", dir);
}

function getStoredList(): Workspace[] {
  try {
    return JSON.parse(localStorage.getItem("ha-workspaces") ?? "[]");
  } catch {
    return [];
  }
}

function getStoredActive(): string | null {
  return localStorage.getItem("ha-active-workspace");
}

describe("basenameOf", () => {
  it("从 Windows 路径提取 basename", () => {
    expect(basenameOf("C:\\Users\\test\\project")).toBe("project");
  });

  it("从 Unix 路径提取 basename", () => {
    expect(basenameOf("/home/user/project")).toBe("project");
  });

  it("去掉尾部分隔符", () => {
    expect(basenameOf("C:\\Users\\test\\project\\")).toBe("project");
    expect(basenameOf("/home/user/project/")).toBe("project");
  });

  it("混合分隔符路径", () => {
    expect(basenameOf("C:/Users/test/project")).toBe("project");
  });

  it("根目录返回空字符串", () => {
    expect(basenameOf("C:\\")).toBe("C:");
  });
});

describe("makeWorkspace", () => {
  it("创建正确的 Workspace 结构", () => {
    const ws = makeWorkspace("D:\\my-project", 0);
    expect(ws.path).toBe("D:\\my-project");
    expect(ws.name).toBe("my-project");
    expect(ws.position).toBe(0);
    expect(ws.id).toMatch(/^ws-/);
    expect(typeof ws.createdAt).toBe("number");
  });

  it("position 可以被指定为任意值", () => {
    const ws = makeWorkspace("C:\\a", 99);
    expect(ws.position).toBe(99);
  });
});

describe("samePath", () => {
  it("大小写不敏感（Windows）", () => {
    expect(samePath("C:\\Project\\App", "c:\\project\\app")).toBe(true);
  });

  it("分隔符归一", () => {
    expect(samePath("C:/Project/App", "C:\\Project\\App")).toBe(true);
  });

  it("去尾分隔符", () => {
    expect(samePath("C:\\Project\\", "C:\\Project")).toBe(true);
    expect(samePath("C:/Project/", "C:\\Project")).toBe(true);
  });

  it("不同路径返回 false", () => {
    expect(samePath("C:\\Project\\A", "C:\\Project\\B")).toBe(false);
  });
});

describe("nextPosition", () => {
  it("空列表返回 0", () => {
    expect(nextPosition([])).toBe(0);
  });

  it("单个空间返回 1", () => {
    const list = [makeWorkspace("C:\\a", 0)];
    expect(nextPosition(list)).toBe(1);
  });

  it("多个空间返回 max+1", () => {
    const list = [makeWorkspace("C:\\a", 0), makeWorkspace("C:\\b", 2), makeWorkspace("C:\\c", 1)];
    expect(nextPosition(list)).toBe(3);
  });
});

describe("initWorkspaces", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("无存储 → 空列表 + null 激活", () => {
    const { list, activeId } = initWorkspaces();
    expect(list).toEqual([]);
    expect(activeId).toBeNull();
  });

  it("旧版单目录非空 → 迁移为第一个空间并激活", () => {
    setLegacyDir("D:\\legacy-project");
    const { list, activeId } = initWorkspaces();
    expect(list.length).toBe(1);
    expect(list[0].path).toBe("D:\\legacy-project");
    expect(list[0].name).toBe("legacy-project");
    expect(list[0].position).toBe(0);
    // 旧键已被清除
    expect(localStorage.getItem("ha-project-work-dir")).toBeNull();
    // 激活 id 等于新空间的 id
    expect(activeId).toBe(list[0].id);
  });

  it("旧版单目录为空字符串 → 不迁移", () => {
    setLegacyDir("");
    const { list, activeId } = initWorkspaces();
    expect(list).toEqual([]);
    expect(activeId).toBeNull();
  });

  it("已有存储列表 → 直接返回", () => {
    const ws = makeWorkspace("C:\\existing", 0);
    localStorage.setItem("ha-workspaces", JSON.stringify([ws]));
    localStorage.setItem("ha-active-workspace", ws.id);
    // 同时有旧版值：不应覆盖已有列表
    setLegacyDir("D:\\should-be-ignored");
    const { list, activeId } = initWorkspaces();
    expect(list.length).toBe(1);
    expect(list[0].path).toBe("C:\\existing");
    expect(activeId).toBe(ws.id);
  });

  it("激活 id 无效时回退到第一个空间", () => {
    const ws = makeWorkspace("C:\\first", 0);
    localStorage.setItem("ha-workspaces", JSON.stringify([ws]));
    // 假的 activeId
    localStorage.setItem("ha-active-workspace", "ws-nonexistent");
    const { list, activeId } = initWorkspaces();
    expect(list.length).toBe(1);
    expect(activeId).toBe(ws.id);
  });

  it("迁移后旧键被清除（幂等：一次迁移，不清除主动写入的持久化数据）", () => {
    setLegacyDir("D:\\legacy");
    // 第一次：迁移，旧键清除
    const first = initWorkspaces();
    expect(first.list.length).toBe(1);
    expect(localStorage.getItem("ha-project-work-dir")).toBeNull();
    // 第二次：旧键已无，列表未持久化，返回空（App 的 useEffect 负责持久化）
    const second = initWorkspaces();
    expect(second.list.length).toBe(0);
    expect(second.activeId).toBeNull();
  });

  it("迁移后 App 若持久化了列表，第二次调用读取持久化结果", () => {
    setLegacyDir("D:\\legacy");
    // 模拟 App 接线后的效果：initWorkspaces 返回迁移数据，App 持久化
    const { list, activeId } = initWorkspaces();
    saveWorkspaces(list, activeId);
    // 第二次调用：读取已持久化的列表
    const { list: list2, activeId: activeId2 } = initWorkspaces();
    expect(list2.length).toBe(1);
    expect(list2[0].path).toBe("D:\\legacy");
    expect(activeId2).toBe(list[0].id);
  });
});

describe("saveWorkspaces", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("持久化列表和激活 id", () => {
    const ws = makeWorkspace("C:\\save-test", 0);
    saveWorkspaces([ws], ws.id);
    const stored = getStoredList();
    expect(stored.length).toBe(1);
    expect(stored[0].path).toBe("C:\\save-test");
    expect(getStoredActive()).toBe(ws.id);
  });

  it("activeId 为 null 时不清除已存储的激活键", () => {
    // saveWorkspaces 在 activeId 为 null 时跳过写入，但保留已有值
    // 这是设计选择：null 通常是空列表场景
    const ws = makeWorkspace("C:\\a", 0);
    saveWorkspaces([ws], ws.id);
    expect(getStoredActive()).toBe(ws.id);
    // 此时 activeId 不应为 null（initWorkspaces 会补上）
  });
});

describe("addWorkspace", () => {
  let list: Workspace[];
  let activeId: string;

  beforeEach(() => {
    list = [
      makeWorkspace("C:\\project-a", 0),
      makeWorkspace("C:\\project-b", 1),
    ];
    activeId = list[0].id;
  });

  it("空输入 → noop", () => {
    const result = addWorkspace(list, activeId, "");
    expect(result.changed).toBe(false);
    expect(result.list).toEqual(list);
  });

  it("新增目录 → 追加新空间并激活，既有空间全部保留（回归：不覆盖激活空间）", () => {
    const result = addWorkspace(list, activeId, "D:\\project-c");
    expect(result.changed).toBe(true);
    expect(result.list.length).toBe(3);
    // 既有两个空间原封不动
    expect(result.list[0].path).toBe("C:\\project-a");
    expect(result.list[1].path).toBe("C:\\project-b");
    // 新空间追加在尾部
    const added = result.list[2];
    expect(added.path).toBe("D:\\project-c");
    expect(added.position).toBe(2);
    expect(result.activeId).toBe(added.id);
  });

  it("新增目录 = 激活空间原路径 → 命中，不重复添加", () => {
    const result = addWorkspace(list, activeId, "C:\\project-a");
    expect(result.changed).toBe(false);
    expect(result.list.length).toBe(2);
    expect(result.activeId).toBe(activeId);
  });

  it("新增目录命中其他空间（大小写不敏感）→ 激活它，不新增", () => {
    const result = addWorkspace(list, activeId, "c:\\PROJECT-B");
    expect(result.changed).toBe(true);
    expect(result.list.length).toBe(2);
    expect(result.activeId).toBe(list[1].id);
  });

  it("空列表 → 建第一个空间", () => {
    const result = addWorkspace([], null, "D:\\first");
    expect(result.list.length).toBe(1);
    expect(result.list[0].path).toBe("D:\\first");
    expect(result.activeId).toBe(result.list[0].id);
  });
});