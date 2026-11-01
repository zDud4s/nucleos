import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { EngineOptIn } from "./EngineOptIn";
import { ApiRefusal } from "../data/client";
import { renderWithQuery } from "../test/harness";

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
