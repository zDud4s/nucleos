// @vitest-environment node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// @ts-expect-error -- a plain .mjs config with no declaration file; typed at the call below.
import remoteConfig from "../../remote.vite.config.mjs";

const shellRoot = fileURLToPath(new URL("../..", import.meta.url));
const srcRoot = path.join(shellRoot, "src");

function sources(dir: string, out: string[] = []): string[] {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (full === path.join(srcRoot, "preview") || full === path.join(srcRoot, "remote")) continue;
      sources(full, out);
    } else if (/\.(ts|tsx)$/.test(entry.name) && !/\.test\.(ts|tsx)$/.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

describe("the remote build", () => {
  it("remote build writes dist-remote, never dist, from remote.html and aliases every tauri specifier", async () => {
    const config = await (remoteConfig as (env: object) => Promise<Record<string, any>>)({
      command: "build",
      mode: "production",
    });

    expect(config.build.outDir).toBe("dist-remote");
    expect(config.build.emptyOutDir).toBe(true);
    expect(path.basename(config.build.rollupOptions.input)).toBe("remote.html");

    const aliases = Object.keys(config.resolve.alias as Record<string, string>);
    const used = new Set<string>();
    for (const file of sources(srcRoot)) {
      const text = fs.readFileSync(file, "utf8");
      for (const match of text.matchAll(/["'`](@tauri-apps\/[a-z0-9/-]+)["'`]/g)) used.add(match[1]);
    }
    expect(used.size).toBeGreaterThan(0);
    for (const specifier of used) expect(aliases, specifier).toContain(specifier);
  });
});
