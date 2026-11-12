import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const openUrl = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => false,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => undefined) }));
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: (...args: unknown[]) => openUrl(...args),
  openPath: vi.fn(),
}));
vi.mock("@tauri-apps/api/path", () => ({ localDataDir: vi.fn() }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));

import { DictationProvider, useDictation } from "../app/Dictation";
import { findUpdate } from "../data/updater";
import { openFile } from "../data/files";
import { desktopTransport, setTransport } from "../data/transport";
import { editorAvailable, editorRemoteNote, openInVscode } from "../lib/vscode";
import { isHostOnlyError, isRemote } from "./host";
import { remoteTransport } from "./transport";
import { UnavailableRemotely } from "./UnavailableRemotely";

beforeEach(() => {
  invoke.mockReset();
  openUrl.mockReset();
  setTransport(remoteTransport);
});

afterEach(() => {
  setTransport(desktopTransport);
});

describe("host-only features, remotely", () => {
  it("UnavailableRemotely renders the explicit state", () => {
    render(<UnavailableRemotely feature="Dictation" />);
    expect(screen.getByRole("status").textContent).toBe("Dictation is only available on the host machine");
  });

  it("isRemote follows the transport and isHostOnlyError matches by name", () => {
    expect(isRemote()).toBe(true);
    setTransport(desktopTransport);
    expect(isRemote()).toBe(false);
    const e = new Error("x");
    e.name = "HostOnlyError";
    expect(isHostOnlyError(e)).toBe(true);
    expect(isHostOnlyError(new Error("x"))).toBe(false);
  });

  it("dictation (needs a microphone API), folder dialog, updater and open-in-VS-Code do not reach the host when remote", async () => {
    // Dictation: pressing the button reaches no host command; with no microphone API (jsdom, like a
    // plain-HTTP page) it says why instead of staying silent.
    let press: ((memo: boolean) => Promise<void>) | undefined;
    let outcome: unknown = "unset";
    function Probe() {
      const d = useDictation();
      press = d.press;
      outcome = d.outcome;
      return null;
    }
    render(
      <DictationProvider>
        <Probe />
      </DictationProvider>,
    );
    await act(async () => {
      await press!(false);
    });
    expect(invoke).not.toHaveBeenCalled();
    expect(outcome).toEqual({ kind: "mic-error", reason: "unsupported" });

    // Updater: nothing offered, nothing asked of the host.
    await expect(findUpdate()).resolves.toBeNull();

    // Open on disk downloads remotely (see adapters.test.ts); a program is still never opened.
    await expect(openFile("a/run.exe")).rejects.toThrow("a program is never run");

    // VS Code: says it cannot, and opens nothing.
    await expect(openInVscode("/home/x/a.rs", 3)).rejects.toThrow("main machine only");
    expect(openUrl).not.toHaveBeenCalled();
  });

  it("open_in_vscode_says_it_cannot_from_the_browser", async () => {
    const error = await openInVscode("/home/x/a.rs", null).then(
      () => null,
      (e: unknown) => e,
    );
    expect(isHostOnlyError(error)).toBe(true);
    expect((error as Error).message).toBe("VS Code opens on the main machine only, not from this browser.");
    expect(editorAvailable()).toBe(false);
    expect(editorRemoteNote()).toBe((error as Error).message);
    setTransport(desktopTransport);
    expect(editorAvailable()).toBe(true);
    expect(editorRemoteNote()).toBeNull();
  });
});
