import { spawn } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const DEV_URL = "http://127.0.0.1:14200/";
const HARNESS_TITLE = /<title>\s*Harness Agent\s*<\/title>/i;
const VITE_ENTRYPOINT = /["']\/src\/main\.tsx(?:\?[^"']*)?["']/;

export async function probeHarnessVite({
  fetchImpl = fetch,
  timeoutMs = 1_000,
  url = DEV_URL,
} = {}) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const response = await fetchImpl(url, { signal: controller.signal });
    const body = await response.text();

    if (response.ok && HARNESS_TITLE.test(body) && VITE_ENTRYPOINT.test(body)) {
      return { kind: "harness" };
    }

    return {
      kind: "foreign",
      detail: response.ok ? "response does not match Harness Vite" : `HTTP ${response.status}`,
    };
  } catch (error) {
    return {
      kind: "unavailable",
      detail: error instanceof Error ? error.message : String(error),
    };
  } finally {
    clearTimeout(timeout);
  }
}

function viteCommand() {
  // Windows 的 npm 暴露为 npm.cmd（批处理文件），Node 的 spawn 无法直接执行它：
  // CreateProcess 只认 .exe，spawn("npm.cmd") 会报 EINVAL，必须经 cmd.exe /c 解释。
  // POSIX 下 npm 是带 shebang 的脚本，可直接 spawn。
  return process.platform === "win32"
    ? [process.env.ComSpec || "cmd.exe", ["/d", "/s", "/c", "npm run dev"]]
    : ["npm", ["run", "dev"]];
}

function runVite() {
  return new Promise((resolveRun, rejectRun) => {
    const [command, args] = viteCommand();
    const child = spawn(command, args, {
      cwd: process.cwd(),
      stdio: "inherit",
    });

    const forwardSignal = (signal) => {
      if (!child.killed) {
        child.kill(signal);
      }
    };
    const onSigint = () => forwardSignal("SIGINT");
    const onSigterm = () => forwardSignal("SIGTERM");
    const cleanup = () => {
      process.removeListener("SIGINT", onSigint);
      process.removeListener("SIGTERM", onSigterm);
    };

    process.once("SIGINT", onSigint);
    process.once("SIGTERM", onSigterm);
    child.once("error", (error) => {
      cleanup();
      rejectRun(error);
    });
    child.once("exit", (code, signal) => {
      cleanup();
      resolveRun({ code, signal });
    });
  });
}

async function main() {
  const probe = await probeHarnessVite();

  if (probe.kind === "harness") {
    console.log("Harness Vite server already running at http://127.0.0.1:14200; reusing it.");
    return;
  }

  if (probe.kind === "foreign") {
    console.error(
      `Cannot start Harness Vite: ${DEV_URL} is occupied by a different service (${probe.detail}).`,
    );
    process.exitCode = 1;
    return;
  }

  console.log("Harness Vite server is not running; starting it now.");
  const result = await runVite();
  process.exitCode = result.code ?? (result.signal ? 1 : 0);
}

const scriptPath = fileURLToPath(import.meta.url);
if (process.argv[1] && resolve(process.argv[1]) === scriptPath) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
