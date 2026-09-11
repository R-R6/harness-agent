import { useEffect, useState } from "react";
import { fetchAgentCatalog } from "./api";
import type { AgentCatalogEntry } from "../types";

/**
 * Agent 注册表状态（挂载拉取一次）。
 * Promise.resolve + Array.isArray 防御：invoke 在测试桩/异常环境可能返回非 promise
 * 或 undefined；拉取失败静默降级，调用方用各自的静态兜底。
 */
export function useAgentCatalog(): AgentCatalogEntry[] {
  const [catalog, setCatalog] = useState<AgentCatalogEntry[]>([]);
  useEffect(() => {
    let alive = true;
    Promise.resolve(fetchAgentCatalog())
      .then((entries) => {
        if (alive && Array.isArray(entries)) setCatalog(entries);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);
  return catalog;
}
