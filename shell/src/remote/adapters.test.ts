import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

// The remote build aliases the plugin to its browser adapter; do the same for the toast under test.
vi.mock("@tauri-apps/plugin-notification", () => import("./adapters/notification"));

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

  describe("task 5.3: notifications, dropped files, downloads", () => {
    afterEach(async () => {
      vi.unstubAllGlobals();
      const { desktopTransport, setTransport } = await import("../data/transport");
      setTransport(desktopTransport);
    });

    async function goRemote() {
      const { setTransport } = await import("../data/transport");
      const { remoteTransport } = await import("./transport");
      setTransport(remoteTransport);
    }

    it("the_remote_build_asks_for_notification_permission_from_a_gesture", async () => {
      await goRemote();
      const toast = await import("../app/desktop-toast");
      const note = { key: "k", title: "Approval waiting", body: "b", to: "/waiting" };
      const request = vi.fn(async () => "granted");
      const shown = vi.fn();
      const Api = Object.assign(shown, { permission: "default", requestPermission: request });
      vi.stubGlobal("Notification", Api);

      // On load and on a toast: never asks, never shows, never fails.
      expect(toast.browserNotificationState()).toBe("default");
      await expect(toast.sendDesktopToast(note)).resolves.toBeUndefined();
      expect(request).not.toHaveBeenCalled();
      expect(shown).not.toHaveBeenCalled();

      // From the click: asks exactly once.
      await expect(toast.askBrowserNotifications()).resolves.toBe("granted");
      expect(request).toHaveBeenCalledTimes(1);

      // Granted: the next toast shows.
      Api.permission = "granted";
      await toast.sendDesktopToast(note);
      expect(shown).toHaveBeenCalledWith("Approval waiting", { body: "b" });

      // Denied or unsupported: no error, nothing shown.
      shown.mockClear();
      Api.permission = "denied";
      await expect(toast.sendDesktopToast(note)).resolves.toBeUndefined();
      expect(shown).not.toHaveBeenCalled();
      vi.stubGlobal("Notification", undefined);
      expect(toast.browserNotificationState()).toBe("unsupported");
      await expect(toast.sendDesktopToast(note)).resolves.toBeUndefined();
      await expect(toast.askBrowserNotifications()).resolves.toBe("denied");
    });

    it("a_dropped_file_is_read_with_the_browser_file_api", async () => {
      const files = await import("../data/files");
      const bytes = new Uint8Array([1, 2, 3]).buffer;
      const good = { name: "a.txt", size: 3, arrayBuffer: async () => bytes } as unknown as File;
      const locked = {
        name: "b.txt",
        size: 9,
        arrayBuffer: async () => {
          throw new DOMException("gone", "NotFoundError");
        },
      } as unknown as File;
      const transfer: import("../data/files").DropTransfer = {
        items: [
          { kind: "file", getAsFile: () => good },
          { kind: "file", getAsFile: () => locked },
          { kind: "file", getAsFile: () => null, webkitGetAsEntry: () => ({ isDirectory: true, name: "Docs" }) },
          { kind: "string", getAsFile: () => null },
        ],
      };
      const [a, b, folder, ...rest] = files.browserDropFiles(transfer);
      expect(rest).toEqual([]);
      expect([a?.name, a?.size, b?.name, folder?.name]).toEqual(["a.txt", 3, "b.txt", "Docs"]);
      await expect(a!.read()).resolves.toBe(bytes);
      await expect(b!.read()).rejects.toThrow("the browser could not read this file");
      await expect(folder!.read()).rejects.toThrow("a folder cannot be sent from a browser");

      // No items API: the plain file list is read the same way.
      expect(files.browserDropFiles({ files: [good] }).map((f) => f.name)).toEqual(["a.txt"]);

      // Only a remote device takes the DOM drop; the desktop keeps the host's events.
      expect(files.browserDropsApply()).toBe(false);
      await goRemote();
      expect(files.browserDropsApply()).toBe(true);
    });

    it("open_on_disk_downloads_in_the_browser", async () => {
      await goRemote();
      const files = await import("../data/files");
      const fetchMock = vi.fn(async () => new Response(new Blob(["hello"]), { status: 200 }));
      vi.stubGlobal("fetch", fetchMock);
      const clicked: Array<{ name: string; href: string }> = [];
      const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
        clicked.push({ name: this.download, href: this.getAttribute("href") ?? "" });
      });

      await files.openFile("Docs/guia final.pdf");

      // A native download: no fetch, no Blob in memory; the browser streams the anchor's URL to disk.
      expect(fetchMock).not.toHaveBeenCalled();
      expect(clicked).toEqual([{ name: "guia final.pdf", href: "/api/files/download?path=Docs%2Fguia%20final.pdf" }]);

      // A program is still never run from the files folder.
      await expect(files.openFile("Docs/setup.exe")).rejects.toThrow("a program is never run");
      expect(clicked).toHaveLength(1);
      click.mockRestore();
    });

    it("a_notification_click_focuses_the_tab_and_follows_its_route", async () => {
      await goRemote();
      const notification = await import("./adapters/notification");
      const made: Array<{ onclick: (() => void) | null; close: () => void }> = [];
      const Granted = Object.assign(
        function () {
          const n = { onclick: null as (() => void) | null, close: vi.fn() };
          made.push(n);
          return n;
        },
        { permission: "granted", requestPermission: vi.fn(async () => "granted") },
      );
      vi.stubGlobal("Notification", Granted);
      const focus = vi.spyOn(window, "focus").mockImplementation(() => {});
      const assign = vi.fn();
      vi.stubGlobal("location", { ...window.location, assign });

      notification.sendNotification({ title: "t", to: "/waiting" });
      made[0]?.onclick?.();
      expect(focus).toHaveBeenCalledTimes(1);
      expect(assign).toHaveBeenCalledWith("/waiting");

      // A route that leaves the app is never followed; the tab is still focused.
      notification.sendNotification({ title: "t", to: "//evil.example/x" });
      made[1]?.onclick?.();
      notification.sendNotification({ title: "t" });
      made[2]?.onclick?.();
      expect(assign).toHaveBeenCalledTimes(1);
      expect(focus).toHaveBeenCalledTimes(3);
      focus.mockRestore();
    });

    it("a_file_dropped_outside_a_drop_zone_does_not_leave_the_app", async () => {
      await goRemote();
      const { installDropGuard } = await import("./drop-guard");
      const remove = installDropGuard();
      for (const type of ["dragover", "drop"]) {
        const event = new Event(type, { cancelable: true, bubbles: true });
        document.body.dispatchEvent(event);
        expect(event.defaultPrevented).toBe(true);
      }
      // A handler lower down still runs: the guard only prevents the default, it stops nothing.
      const seen = vi.fn();
      document.body.addEventListener("drop", seen);
      document.body.dispatchEvent(new Event("drop", { cancelable: true, bubbles: true }));
      expect(seen).toHaveBeenCalledTimes(1);
      document.body.removeEventListener("drop", seen);
      remove();
      const after = new Event("drop", { cancelable: true, bubbles: true });
      document.body.dispatchEvent(after);
      expect(after.defaultPrevented).toBe(false);
    });
  });
});
