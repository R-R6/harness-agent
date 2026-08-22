#!/usr/bin/env node
/**
 * agent-sessions-mcp —— 把本地 AI Agent 的会话记录暴露为 MCP 工具
 *
 * 适配器（adapter 模式，未来可加 cursor 等）：
 *  - claude: ~/.claude/projects/<项目slug>/<uuid>.jsonl
 *  - codex : ~/.codex/sessions/<年>/<月>/<日>/rollout-*.jsonl
 *
 * 工具：
 *  - list_sessions({ agent? })        列出最近会话（各 agent 前 20 个）
 *  - get_transcript({ file, tail?, offset? }) 读取某会话正文（默认尾部 200 行；offset 从末尾跳过 N 条，往前翻页）
 *  - search_sessions({ keyword })     按关键词搜索会话文件
 *
 * 传输：stdio（JSON-RPC 2.0，每行一个 JSON 对象；stdout 只能输出协议行，
 *       诊断信息一律走 stderr）。
 *
 * ⚠️ 安全：会话 JSONL 含完整对话（可能有密钥/代码），本 server 仅限本机
 *    stdio 使用，不要暴露为远程 HTTP 服务。
 */
'use strict';

const fs = require('fs');
const path = require('path');
const readline = require('readline');

const HOME = process.env.USERPROFILE || process.env.HOME || '';
const MAX_LINES = 200;                 // get_transcript 默认行数
// 单文件读取上限（默认 8MB，防超大文件撑爆内存；长会话可用 AGENT_SESSIONS_MAX_BYTES 覆盖）
const MAX_FILE_BYTES = parseInt(process.env.AGENT_SESSIONS_MAX_BYTES, 10) || 8 * 1024 * 1024;
const LIST_LIMIT = 20;                 // list_sessions 每 agent 条数

// ---------------- 文件遍历 ----------------

function walkJsonl(root, { excludeDirs = [] } = {}) {
  const out = [];
  if (!fs.existsSync(root)) return out;
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      const p = path.join(dir, e.name);
      if (e.isDirectory()) {
        if (!excludeDirs.includes(e.name)) stack.push(p);
      } else if (e.name.endsWith('.jsonl')) {
        try {
          out.push({ file: p, mtime: fs.statSync(p).mtimeMs });
        } catch { /* 忽略瞬时错误 */ }
      }
    }
  }
  return out.sort((a, b) => b.mtime - a.mtime);
}

// ---------------- 解析器（容忍未知行类型） ----------------

function parseClaude(obj) {
  const t = obj && obj.type;
  if (t === 'ai-title' || t === 'custom-title') {
    return { type: 'title', text: obj.aiTitle || obj.customTitle || '' };
  }
  if (t === 'user' || t === 'assistant') {
    const msg = obj.message || {};
    const role = t === 'assistant' ? 'assistant' : 'user';
    if (Array.isArray(msg.content)) {
      const parts = [];
      for (const c of msg.content) {
        if (typeof c === 'string') parts.push(c);
        else if (c && typeof c.text === 'string') parts.push(c.text);
        else if (c && c.type === 'tool_use')
          parts.push(`[tool_use ${c.name}] ${JSON.stringify(c.input || {})}`);
        else if (c && c.type === 'tool_result')
          parts.push(`[tool_result] ${formatToolContent(c.content)}`);
      }
      return { type: role, text: parts.join('\n') };
    }
    return { type: role, text: typeof msg.content === 'string' ? msg.content : JSON.stringify(msg.content) };
  }
  return null;
}

/// 工具结果 content 序列化。真实 Claude JSONL 里 content 可能是字符串、文本块数组
/// （[{type:'text',text:...}]）或嵌套结构；直接 String() 会把对象数组变成 "[object Object]"
function formatToolContent(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((p) => typeof p === 'string' ? p : p && typeof p.text === 'string' ? p.text : JSON.stringify(p))
      .join('\n');
  }
  return JSON.stringify(content);
}

function parseCodex(obj) {
  const t = obj && obj.type;
  if (t === 'response_item' && obj.payload) {
    const p = obj.payload;
    const role = p.role === 'assistant' ? 'assistant' : p.role === 'user' ? 'user' : 'system';
    if (Array.isArray(p.content)) {
      const parts = [];
      for (const c of p.content) {
        if (c && typeof c.text === 'string') parts.push(c.text);
      }
      if (parts.length) return { type: role, text: parts.join('\n') };
    }
    return null;
  }
  if (t === 'event_msg' && obj.payload) {
    const p = obj.payload;
    if (p.type === 'agent_message') return { type: 'assistant', text: p.message || '' };
    if (p.type === 'user_message') return { type: 'user', text: p.message || '' };
    if (p.type === 'token_count')
      return { type: 'usage', text: JSON.stringify(p.info && p.info.total_token_usage || p.info || {}) };
  }
  return null;
}

function parseEither(obj) {
  return parseClaude(obj) || parseCodex(obj) || null;
}

// ---------------- 适配器 ----------------

// 数据根目录：默认指向真实会话目录；可用环境变量覆盖（模拟/测试用，不影响真实环境）
const CLAUDE_ROOT = process.env.AGENT_SESSIONS_CLAUDE_ROOT || path.join(HOME, '.claude', 'projects');
const CODEX_ROOT = process.env.AGENT_SESSIONS_CODEX_ROOT || path.join(HOME, '.codex', 'sessions');

const adapters = {
  claude: {
    label: 'Claude Code',
    files: () => walkJsonl(CLAUDE_ROOT, {
      excludeDirs: ['subagents', 'tool-results'],
    }),
  },
  codex: {
    label: 'Codex',
    files: () => walkJsonl(CODEX_ROOT),
  },
};

// 本地时间 ISO（带时区偏移，如 2026-08-07T19:36:10+08:00）——toISOString() 固定输出 UTC，对非 UTC 用户不友好
function localISO(d) {
  const off = -d.getTimezoneOffset();
  const p = (n) => String(n).padStart(2, '0');
  const base = d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) + 'T' +
    p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
  const sign = off >= 0 ? '+' : '-';
  return base + sign + p(Math.floor(Math.abs(off) / 60)) + ':' + p(Math.abs(off) % 60);
}

// ---------------- 工具实现 ----------------

// 路径白名单（S1 加固）：get_transcript 只允许读会话根目录内的文件，防任意文件读取
// 规范化路径前缀匹配（大小写不敏感，Windows）
function isWithinSessionsRoot(file) {
  const norm = path.normalize(file).toLowerCase();
  return [CLAUDE_ROOT, CODEX_ROOT].some(root =>
    norm === root.toLowerCase() || norm.startsWith(root.toLowerCase() + path.sep)
  );
}

function listSessions(params) {
  const { agent, limit } = params || {};
  // limit 可配置（默认 20，上限 200）
  const n = Math.min(Math.max(parseInt(limit, 10) || LIST_LIMIT, 1), 200);
  const rows = [];
  const names = agent ? [agent] : Object.keys(adapters);
  for (const name of names) {
    const a = adapters[name];
    if (!a) continue;
    for (const f of a.files().slice(0, n)) {
      rows.push({
        agent: name,
        agentLabel: a.label,
        file: f.file,
        title: extractTitle(f.file),   // 会话标题（列表展示用，空则前端回退文件名）
        updated: localISO(new Date(f.mtime)),
      });
    }
  }
  rows.sort((x, y) => y.updated.localeCompare(x.updated));
  return rows.slice(0, n * 2);
}

/// 从会话文件头部提取标题（Claude: ai-title/custom-title；Codex: session_meta.title）。
/// 只读文件前 64KB，避免大文件开销；读不到返回空串。
function extractTitle(file) {
  try {
    const fd = fs.openSync(file, 'r');
    const buf = Buffer.alloc(64 * 1024);
    const n = fs.readSync(fd, buf, 0, buf.length, 0);
    fs.closeSync(fd);
    for (const raw of buf.slice(0, n).toString('utf8').split('\n')) {
      if (!raw.trim()) continue;
      let obj;
      try {
        obj = JSON.parse(raw);
      } catch {
        continue;
      }
      const t = obj && obj.type;
      if (t === 'ai-title' && obj.aiTitle) return String(obj.aiTitle).slice(0, 120);
      if (t === 'custom-title' && obj.customTitle) return String(obj.customTitle).slice(0, 120);
      if (t === 'session_meta' && obj.title) return String(obj.title).slice(0, 120);
    }
    return '';
  } catch {
    return '';
  }
}

function getTranscript(params) {
  const { file, tail, offset } = params || {};
  if (!file || typeof file !== 'string') throw new Error('参数 file 必填');
  if (!isWithinSessionsRoot(file)) throw new Error('拒绝访问：文件不在会话根目录内（list_sessions 返回的 file 路径才可读）');
  if (!fs.existsSync(file)) throw new Error('文件不存在: ' + file);
  const n = Math.min(Math.max(parseInt(tail, 10) || MAX_LINES, 1), 2000);
  // offset：从末尾跳过 N 条再取 tail 条，配合 tail 往前翻页直到会话开头
  const skip = Math.max(parseInt(offset, 10) || 0, 0);
  const stat = fs.statSync(file);
  if (stat.size > MAX_FILE_BYTES) throw new Error(`文件过大（>${Math.round(MAX_FILE_BYTES / 1024 / 1024)}MB），请改用 search_sessions 定位`);
  const text = fs.readFileSync(file, 'utf8');
  const out = [];
  for (const raw of text.split('\n')) {
    if (!raw.trim()) continue;
    let obj;
    try {
      obj = JSON.parse(raw);
    } catch {
      continue;
    }
    const item = parseEither(obj);
    if (!item) continue;
    out.push({
      type: item.type,
      text: String(item.text || ''),
      at: obj.timestamp || undefined,
    });
  }
  if (skip >= out.length) return []; // 已翻到会话最前
  return out.slice(Math.max(0, out.length - skip - n), out.length - skip);
}

function searchSessions(params) {
  const { keyword, limit } = params || {};
  if (!keyword || typeof keyword !== 'string') throw new Error('参数 keyword 必填');
  const kw = keyword.toLowerCase();
  // limit 可配置（默认 20，上限 200）
  const n = Math.min(Math.max(parseInt(limit, 10) || LIST_LIMIT, 1), 200);
  const hits = [];
  for (const name of Object.keys(adapters)) {
    for (const f of adapters[name].files()) {
      try {
        if (fs.statSync(f.file).size > MAX_FILE_BYTES) continue;
        const text = fs.readFileSync(f.file, 'utf8');
        // 只搜对话内容（user/assistant/title），跳过工具输出/环境上下文杂音：
        // 原始全文 includes 会把 custom_tool_call_output（脚本 stdout）、
        // system 提示等搜进来，导致"无关会话"命中（2026-08-13 用户反馈修正）
        let hit = false;
        for (const raw of text.split('\n')) {
          if (!raw.trim()) continue;
          let obj;
          try {
            obj = JSON.parse(raw);
          } catch {
            continue;
          }
          if (!isSearchableContent(obj)) continue;
          const item = parseEither(obj);
          if (item && item.text && item.text.toLowerCase().includes(kw)) {
            hit = true;
            break;
          }
        }
        if (!hit) continue;
        // 与 list_sessions 保持一致：返回 agentLabel（契约一致，2026-08-13 补）
        hits.push({ agent: name, agentLabel: adapters[name].label, file: f.file, updated: localISO(new Date(f.mtime)) });
        if (hits.length >= n * 2) return hits;
      } catch { /* 跳过读不了的文件 */ }
    }
  }
  return hits;
}

/// 判断一行是否属于"可搜索的对话内容"（排除工具输出/环境上下文/用量记录）
function isSearchableContent(obj) {
  const t = obj && obj.type;
  // Claude：user / assistant / 标题
  if (t === 'user' || t === 'assistant' || t === 'ai-title' || t === 'custom-title') return true;
  // Codex：对话消息（response_item.message）与 agent/user_message 事件
  if (t === 'response_item' && obj.payload && obj.payload.type === 'message') return true;
  if (t === 'event_msg' && obj.payload && (obj.payload.type === 'agent_message' || obj.payload.type === 'user_message')) return true;
  return false;
}

// ---------------- JSON-RPC 分发 ----------------

const TOOLS = [
  {
    name: 'list_sessions',
    description: '列出本机 AI Agent 的最近会话（Claude Code / Codex），返回文件路径与更新时间',
    inputSchema: {
      type: 'object',
      properties: { agent: { type: 'string', enum: ['claude', 'codex'], description: '可选，只列某个 agent' } },
    },
  },
  {
    name: 'get_transcript',
    description: '读取某会话文件的正文（用户/助手消息、工具调用、token 用量）',
    inputSchema: {
      type: 'object',
      properties: {
        file: { type: 'string', description: 'list_sessions 返回的 file 路径' },
        tail: { type: 'number', description: '可选，只取末尾 N 条（默认 200，上限 2000）' },
        offset: { type: 'number', description: '可选，从末尾跳过 N 条再取（配合 tail 往前翻页到会话开头，默认 0）' },
      },
      required: ['file'],
    },
  },
  {
    name: 'search_sessions',
    description: '按关键词全文搜索会话文件（不区分大小写），返回匹配的文件列表',
    inputSchema: {
      type: 'object',
      properties: { keyword: { type: 'string' } },
      required: ['keyword'],
    },
  },
];

function callTool(name, params) {
  switch (name) {
    case 'list_sessions': return listSessions(params);
    case 'get_transcript': return getTranscript(params);
    case 'search_sessions': return searchSessions(params);
    default: throw new Error('unknown tool: ' + name);
  }
}

function handle(req) {
  const { id, method, params } = req || {};
  if (method === 'initialize') {
    // 回显客户端请求的协议版本（互操作关键：不同客户端支持的版本不同，硬编码会握手失败）
    const clientVersion = params && params.protocolVersion;
    return {
      protocolVersion: typeof clientVersion === 'string' && clientVersion.length > 0 ? clientVersion : '2025-06-18',
      capabilities: { tools: {} },
      serverInfo: { name: 'agent-sessions-mcp', version: '0.1.0' },
    };
  }
  if (method === 'tools/list') return { tools: TOOLS };
  // 能力查询：没有资源/提示词就返回空列表（返回错误会被客户端视为 server 不健康）
  if (method === 'resources/list') return { resources: [] };
  if (method === 'resources/templates/list') return { resourceTemplates: [] };
  if (method === 'prompts/list') return { prompts: [] };
  if (method === 'tools/call') {
    const result = callTool(params && params.name, params && params.arguments);
    // 必须紧凑单行 JSON：MCP stdio 以换行分帧，美化输出（含真实换行）会破坏协议
    return { content: [{ type: 'text', text: JSON.stringify(result) }] };
  }
  if (method === 'ping') return {};
  if (typeof method === 'string' && method.startsWith('notifications/')) return null; // 通知类无响应
  throw new Error('method not found: ' + method); // 其余未知方法返回 JSON-RPC 错误
}

// ---------------- stdio 入口 ----------------

const rl = readline.createInterface({ input: process.stdin });

rl.on('line', line => {
  let req;
  try {
    req = JSON.parse(line);
  } catch {
    return; // 忽略坏行
  }
  try {
    const result = handle(req);
    if (result !== null && req.id !== undefined) {
      // JSON-RPC 2.0 规范：每条消息必须带 "jsonrpc":"2.0" 字段（codex 的 rmcp 客户端严格要求，缺失会被拒）
      process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: req.id, result }) + '\n');
    }
  } catch (e) {
    if (req.id !== undefined) {
      process.stdout.write(
        JSON.stringify({ jsonrpc: '2.0', id: req.id, result: { isError: true, content: [{ type: 'text', text: String(e.message || e) }] } }) + '\n'
      );
    }
  }
});

// stdin 结束后不主动 process.exit()：自然退出会让 Node 先 flush 完 stdout 管道缓冲，
// 主动 exit 可能截断尚未写入的协议响应。

