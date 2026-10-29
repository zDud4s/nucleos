import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

const SRC = join(__dirname, "..");

const ADAPTERS: Record<string, string> = {
  "@tauri-apps/api/core": "core",
  "@tauri-apps/api/event": "event",
  "@tauri-apps/api/window": "window",
  "@tauri-apps/api/path": "path",
  "@tauri-apps/plugin-opener": "opener",
  "@tauri-apps/plugin-notification": "notification",
  "@tauri-apps/plugin-dialog": "dialog",
  "@tauri-apps/plugin-process": "process",
  "@tauri-apps/plugin-updater": "updater",
};

function sources(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    const rel = relative(SRC, full).split(sep).join("/");
    if (entry.isDirectory()) {
      if (rel === "preview" || rel === "remote") continue;
      out.push(...sources(full));
    } else if (/\.tsx?$/.test(entry.name) && !/\.test\./.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

describe("remote adapters", () => {
  it("every tauri import in non-test source resolves to an exported name of its adapter", async () => {
    const wanted = new Map<string, Set<string>>();
    const re = /import\s+(?!type\b)\{([^}]*)\}\s+from\s+"(@tauri-apps\/[^"]+)"/g;
    for (const file of sources(SRC)) {
      const text = readFileSync(file, "utf8");
      for (const m of text.matchAll(re)) {
        const names = m[1]!
          .split(",")
          .map((n) => n.trim())
          .filter((n) => n !== "" && !n.startsWith("type "))
          .map((n) => n.split(/\s+as\s+/)[0]!.trim());
        const set = wanted.get(m[2]!) ?? new Set<string>();
        names.forEach((n) => set.add(n));
        wanted.set(m[2]!, set);
      }
    }
    expect(wanted.size).toBeGreaterThan(0);
    for (const [specifier, names] of wanted) {
      const adapter = ADAPTERS[specifier];
      expect(adapter, `no adapter for ${specifier}`).toBeDefined();
      const mod = (await import(`./adapters/${adapter}.ts`)) as Record<string, unknown>;
      for (const name of names) {
        expect(mod[name], `${specifier} -> ${name}`).toBeDefined();
      }
    }
  });

  it("host commands reject with HostOnlyError and listen returns a no-op unsubscribe", async () => {
    const core = await import("./adapters/core");
    const event = await import("./adapters/event");
    const path = await import("./adapters/path");
    const win = await import("./adapters/window");
    await expect(core.invoke("do_thing")).rejects.toBeInstanceOf(core.HostOnlyError);
    await expect(core.invoke("do_thing")).rejects.toMatchObject({
      name: "HostOnlyError",
      message: "do_thing is unavailable remotely",
    });
    expect(core.isTauri()).toBe(false);
    await expect(path.localDataDir()).rejects.toBeInstanceOf(core.HostOnlyError);
    const stop = await event.listen("x", () => {});
    expect(stop()).toBeUndefined();
    expect(await event.once("x", () => {})).toBeTypeOf("function");
    await expect(event.emit("x", 1)).resolves.toBeUndefined();
    const own = win.getCurrentWindow();
    await expect(own.hide()).resolves.toBeUndefined();
    await expect(own.innerPosition()).resolves.toBeUndefined();
    await expect(win.currentMonitor()).resolves.toBeNull();
    expect(await own.onMoved(() => {})).toBeTypeOf("function");
  });

  describe("browser APIs", () => {
    afterEach(() => {
      vi.unstubAllGlobals();
    });

    it("opener opens a new window and notifications degrade silently", async () => {
      const opener = await import("./adapters/opener");
      const notification = await import("./adapters/notification");
      const core = await import("./adapters/core");
      const open = vi.fn();
      vi.stubGlobal("window", { open });
      await opener.openUrl("https://example.com/a");
      expect(open).toHaveBeenCalledWith("https://example.com/a", "_blank", "noopener,noreferrer");
      await expect(opener.openUrl("file:///etc/passwd")).rejects.toBeInstanceOf(core.HostOnlyError);
      await expect(opener.openPath("C:/x")).rejects.toBeInstanceOf(core.HostOnlyError);
      expect(open).toHaveBeenCalledTimes(1);

      // No Notification at all.
      vi.stubGlobal("Notification", undefined);
      await expect(notification.isPermissionGranted()).resolves.toBe(false);
      await expect(notification.requestPermission()).resolves.toBe("denied");
      expect(() => notification.sendNotification({ title: "t" })).not.toThrow();

      // Denied: never constructs.
      const ctor = vi.fn();
      const Denied = Object.assign(ctor, { permission: "denied", requestPermission: vi.fn(async () => "denied") });
      vi.stubGlobal("Notification", Denied);
      await expect(notification.isPermissionGranted()).resolves.toBe(false);
      notification.sendNotification({ title: "t" });
      expect(ctor).not.toHaveBeenCalled();

      // Granted: shows one.
      const shown = vi.fn();
      const Granted = Object.assign(shown, { permission: "granted", requestPermission: vi.fn(async () => "granted") });
      vi.stubGlobal("Notification", Granted);
      await expect(notification.isPermissionGranted()).resolves.toBe(true);
      notification.sendNotification({ title: "hi", body: "b" });
      expect(shown).toHaveBeenCalledWith("hi", { body: "b" });
    });
  });
});
