import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import type { QueryClient } from "@tanstack/react-query";
import { EngineOptIn } from "./EngineOptIn";
import { ApiRefusal } from "../data/client";
import { renderWithQuery } from "../test/harness";
import { createAppQueryClient } from "../app/queryClient";
import { keys } from "../data/keys";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

// Fixture sentences on purpose: neither appears anywhere in the shell's source, so seeing them on
// screen proves the shell drew what the daemon sent and owns no copy of either.
const WARNING = "Fixture warning: the lantern engine reads aloud whatever you hand it.";
const TAKES_EFFECT = "Fixture takes_effect: applies when the next zebra crosses.";

interface Opts {
  enabled?: boolean;
  acknowledged?: boolean;
  /** What the PUT does; the default accepts and answers like the daemon. */
  put?: () => Promise<unknown>;
}

function answer(opts: Opts = {}) {
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    if (path !== "/engines/agy") throw new Error(`unexpected ${path}`);
    if (init?.method === "PUT") {
      return opts.put === undefined ? { version: 2, takes_effect: TAKES_EFFECT } : opts.put();
    }
    return {
      enabled: opts.enabled ?? false,
      acknowledged: opts.acknowledged ?? false,
      warning: WARNING,
      warning_version: 1,
      installed: true,
    };
  });
}

function puts(): unknown[][] {
  return daemon.apiFetch.mock.calls.filter((call) => call[1]?.method === "PUT");
}

async function theSwitch(): Promise<HTMLInputElement> {
  return (await screen.findByRole("switch")) as HTMLInputElement;
}

function mount() {
  return renderWithQuery(<EngineOptIn engine="agy" label="Antigravity CLI (agy)" />);
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("EngineOptIn", () => {
  it("agy is off by default and shows no warning until asked", async () => {
    answer();
    mount();

    const toggle = await theSwitch();
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/engines/agy"));
    expect(toggle.checked).toBe(false);
    expect(screen.getByText(/Antigravity CLI \(agy\)/)).toBeDefined();
    expect(screen.queryByText(WARNING)).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(puts()).toHaveLength(0);
  });

  it("enabling shows the daemon's warning and sends nothing until confirmed", async () => {
    answer();
    mount();

    fireEvent.click(await theSwitch());

    expect(await screen.findByRole("dialog")).toBeDefined();
    expect(await screen.findByText(WARNING)).toBeDefined();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Enable" })).toBeDefined();
    expect(puts()).toHaveLength(0);
  });

  it("confirming sends enabled+acknowledged and shows takes_effect", async () => {
    answer();
    mount();

    fireEvent.click(await theSwitch());
    await screen.findByText(WARNING);
    fireEvent.click(screen.getByRole("button", { name: "Enable" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/engines/agy", {
        method: "PUT",
        body: JSON.stringify({ enabled: true, acknowledged: true }),
      }),
    );
    expect(await screen.findByText(TAKES_EFFECT)).toBeDefined();
    expect(puts()).toHaveLength(1);
  });

  it("cancelling sends nothing", async () => {
    answer();
    mount();

    const toggle = await theSwitch();
    fireEvent.click(toggle);
    await screen.findByText(WARNING);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(puts()).toHaveLength(0);
    expect(toggle.checked).toBe(false);
  });

  it("disabling sends enabled false with no dialog", async () => {
    answer({ enabled: true, acknowledged: true });
    mount();

    const toggle = await theSwitch();
    await waitFor(() => expect(toggle.checked).toBe(true));
    fireEvent.click(toggle);

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/engines/agy", {
        method: "PUT",
        body: JSON.stringify({ enabled: false }),
      }),
    );
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByText(WARNING)).toBeNull();
  });

  it("a refusal is shown with the RefusalNote", async () => {
    const detail = "Fixture refusal detail: the lantern was not acknowledged.";
    answer({
      put: () => Promise.reject(new ApiRefusal(400, "warning_not_acknowledged", detail)),
    });
    mount();

    fireEvent.click(await theSwitch());
    await screen.findByText(WARNING);
    fireEvent.click(screen.getByRole("button", { name: "Enable" }));

    // RefusalNote draws the code beside the sentence, and role=status; an ErrorNote would not.
    expect(await screen.findByText("warning_not_acknowledged")).toBeDefined();
    expect(await screen.findByText(detail)).toBeDefined();
  });
});

describe("EngineOptIn - what the window claims (review 7)", () => {
  /** Every menu a chat may read, cached as the front door and an open chat would have them. */
  const MENUS = [
    keys.chats.models,
    keys.chats.modelsNewChat,
    keys.chats.modelsFor("c-1"),
    keys.chats.modelGroups,
    keys.chats.modelGroupsFor("c-1"),
  ];

  function withCachedMenus() {
    const queryClient = createAppQueryClient();
    for (const key of MENUS) queryClient.setQueryData(key, { cached: true });
    return queryClient;
  }

  function invalidated(queryClient: QueryClient): boolean[] {
    return MENUS.map((key) => queryClient.getQueryState(key)?.isInvalidated === true);
  }

  async function enable(): Promise<void> {
    fireEvent.click(await theSwitch());
    await screen.findByText(WARNING);
    fireEvent.click(screen.getByRole("button", { name: "Enable" }));
  }

  it("a successful write invalidates the model menus, which cache for an hour", async () => {
    answer();
    const queryClient = withCachedMenus();
    renderWithQuery(<EngineOptIn engine="agy" label="Antigravity CLI (agy)" />, { queryClient });

    await enable();
    await screen.findByText(TAKES_EFFECT);

    await waitFor(() => expect(invalidated(queryClient)).toEqual(MENUS.map(() => true)));
  });

  it("a failed write invalidates them too, since it may have landed", async () => {
    answer({ put: () => Promise.reject(new Error("socket closed")) });
    const queryClient = withCachedMenus();
    renderWithQuery(<EngineOptIn engine="agy" label="Antigravity CLI (agy)" />, { queryClient });

    await enable();
    await waitFor(() => expect(puts()).toHaveLength(1));

    await waitFor(() => expect(invalidated(queryClient)).toEqual(MENUS.map(() => true)));
  });

  /** A fetch whose PUT fails and whose reads after it wait on `release`; the first read is instant. */
  function failedWriteThenHeldRead(): () => void {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let reads = 0;
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) => {
      if (init?.method === "PUT") throw new Error("socket closed");
      reads += 1;
      if (reads > 1) await held;
      return { enabled: false, acknowledged: false, warning: WARNING, warning_version: 1, installed: true };
    });
    return release;
  }

  // Wording assumed: while the read is pending the note matches /unknown|not sure|may have/i, e.g.
  // "whether {label} was changed is unknown". Once the read settles that note is replaced by one
  // matching /no reply/i, e.g. "the daemon gave no reply to that change: the switch shows the
  // current setting".
  it("a write that got no reply is unknown while the read after it is pending", async () => {
    const release = failedWriteThenHeldRead();
    mount();

    await enable();

    expect(await screen.findByText(/unknown/i)).toBeDefined();
    expect(screen.queryByText(/was not changed/i)).toBeNull();
    release();
  });

  it("once the read after a write with no reply settles, the switch shows the current setting", async () => {
    const release = failedWriteThenHeldRead();
    mount();

    await enable();
    await screen.findByText(/unknown/i);
    release();

    expect(await screen.findByText(/no reply/i)).toBeDefined();
    await waitFor(() => expect(screen.queryByText(/unknown/i)).toBeNull());
    expect(screen.queryByText(/not responding/i)).toBeNull();
    expect(screen.queryByText(/was not changed/i)).toBeNull();
  });

  it("a write that went through and a read that then failed still shows takes_effect", async () => {
    let reads = 0;
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) => {
      if (init?.method === "PUT") return { version: 2, takes_effect: TAKES_EFFECT };
      reads += 1;
      if (reads > 1) throw new Error("socket closed");
      return { enabled: false, acknowledged: false, warning: WARNING, warning_version: 1, installed: true };
    });
    mount();

    await enable();
    expect(await screen.findByText(TAKES_EFFECT)).toBeDefined();
    await waitFor(() => expect(reads).toBeGreaterThan(1));
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });

    expect(screen.getByText(TAKES_EFFECT)).toBeDefined();
    expect(screen.getByText(/could not read/i)).toBeDefined();
    expect(screen.queryByText(/was not changed/i)).toBeNull();
  });

  // Wording assumed: the read note matches /could not read/i, e.g. "the daemon is not responding:
  // could not read the setting for {label}".
  it("a first read that fails offers no switch and does not claim a change was refused", async () => {
    daemon.apiFetch.mockImplementation(async () => {
      throw new Error("socket closed");
    });
    mount();

    expect(await screen.findByText(/could not read/i)).toBeDefined();
    expect(screen.queryByText(/was not changed/i)).toBeNull();
    expect(screen.queryByRole("switch")).toBeNull();
    expect(puts()).toHaveLength(0);
  });
});
