// 版本一致性门禁：界面标签（根 package.json）、Rust/安装包元数据
// （src-tauri 的 workspace.package.version）、内置 MCP server
// （resources package.json）必须一致。任何一处漂移直接构建失败，
// 防止"界面 0.2.0 / 安装包 0.1.0"这类分叉再次溜进发布。
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function readJson(path) {
  return JSON.parse(readFileSync(join(root, path), "utf-8"));
}

function readCargoWorkspaceVersion() {
  const text = readFileSync(join(root, "src-tauri", "Cargo.toml"), "utf-8");
  const m = text.match(/\[workspace\.package\][^\[]*?version\s*=\s*"([^"]+)"/s);
  return m?.[1];
}

function readTauriConfVersion() {
  // tauri.conf.json 的 version 已移除（回退 Cargo.toml）；若未来有人加回来，
  // 仍纳入校验，避免出现第三处手工版本
  const conf = readJson("src-tauri/tauri.conf.json");
  return conf.version;
}

const expected = readJson("package.json").version;
const sources = {
  "package.json": expected,
  "src-tauri Cargo workspace": readCargoWorkspaceVersion(),
  "resources/agent-sessions-mcp/package.json": readJson(
    "src-tauri/resources/agent-sessions-mcp/package.json",
  ).version,
  "src-tauri/tauri.conf.json（可选，回退 Cargo）": readTauriConfVersion(),
};

let ok = true;
for (const [name, version] of Object.entries(sources)) {
  if (version === undefined) continue; // 可选来源缺省 = 继承/回退，视为一致
  if (version !== expected) {
    ok = false;
    console.error(`✗ ${name}: ${version} ≠ package.json: ${expected}`);
  }
}

if (!ok) {
  console.error("\n版本号不一致：请把所有清单改同后重试（UI 标签读 package.json，安装包元数据读 Cargo workspace）");
  process.exit(1);
}
console.log(`✓ 版本一致：v${expected}（package.json / Cargo workspace / 内置 server）`);
