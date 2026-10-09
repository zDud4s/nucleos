// @vitest-environment node
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const moduleUrl = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
const conf = JSON.parse(
  readFileSync(fileURLToPath(new URL("../src-tauri/tauri.conf.json", moduleUrl)), "utf8"),
) as { app: { windows: { label?: string; minWidth?: number; minHeight?: number }[] } };

describe("main window size", () => {
  it("sets a minimum of at least 960 by 640", () => {
    const main = conf.app.windows.find((window) => window.label === undefined || window.label === "main");
    expect(main).toBeDefined();
    expect(main?.minWidth ?? 0).toBeGreaterThanOrEqual(960);
    expect(main?.minHeight ?? 0).toBeGreaterThanOrEqual(640);
  });
});
