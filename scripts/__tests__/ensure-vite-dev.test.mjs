import { describe, expect, it, vi } from "vitest";

import { probeHarnessVite } from "../ensure-vite-dev.mjs";

describe("probeHarnessVite", () => {
  it("recognizes the Harness Vite page", async () => {
    const fetchImpl = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      text: () =>
        Promise.resolve(
          '<!doctype html><title>Harness Agent</title><script type="module" src="/src/main.tsx"></script>',
        ),
    });

    await expect(probeHarnessVite({ fetchImpl })).resolves.toMatchObject({
      kind: "harness",
    });
  });

  it("recognizes the Harness Vite page when HMR adds an entrypoint timestamp", async () => {
    const fetchImpl = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      text: () =>
        Promise.resolve(
          '<!doctype html><title>Harness Agent</title><script type="module" src="/src/main.tsx?t=1786782597350"></script>',
        ),
    });

    await expect(probeHarnessVite({ fetchImpl })).resolves.toMatchObject({
      kind: "harness",
    });
  });

  it("rejects an unrelated service on the development port", async () => {
    const fetchImpl = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      text: () => Promise.resolve("<title>Another App</title>"),
    });

    await expect(probeHarnessVite({ fetchImpl })).resolves.toMatchObject({
      kind: "foreign",
    });
  });

  it("allows Vite to start when the development port is unavailable", async () => {
    const connectionError = new Error("connect ECONNREFUSED");
    connectionError.cause = { code: "ECONNREFUSED" };
    const fetchImpl = vi.fn().mockRejectedValue(connectionError);

    await expect(probeHarnessVite({ fetchImpl })).resolves.toMatchObject({
      kind: "unavailable",
    });
  });
});
