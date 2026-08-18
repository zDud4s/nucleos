import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import { ConnectionGate } from "./ConnectionGate";
import { ApiRefusal, ApiUnavailable } from "../data/client";
import { renderWithQuery } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

function shell() {
  return <p>the shell</p>;
}

async function takeoverText(): Promise<string> {
  const alert = await screen.findByRole("alert");
  return alert.textContent ?? "";
}

/**
 * The one distinction this component exists for.
 *
 * "Cannot connect" is two completely different pieces of news wearing one
 * sentence. No answer at all clears by waiting, and the shell should say so.
 * An answer that was *no* never clears by waiting — the daemon token rotates
 * when the daemon restarts, and a window holding the old one will be refused
 * for as long as it stays open. A person told to wait for that spends an
 * afternoon on it.
 */
describe("ConnectionGate", () => {
  it("takes the window over when the daemon is not answering", async () => {
    daemon.probeHealth.mockResolvedValue(false);

    renderWithQuery(<ConnectionGate>{shell()}</ConnectionGate>);

    const text = await takeoverText();
    expect(text).toMatch(/unreachable/i);
    // Waiting is the whole treatment here, so the screen says it is waiting.
    expect(text).toMatch(/retrying/i);
    expect(screen.queryByText("the shell")).toBeNull();
  });

  it("takes the window over differently when the daemon refuses the credential", async () => {
    daemon.probeHealth.mockResolvedValue(true);
    // A locked or empty keychain: the daemon may be perfectly healthy, and we
    // never got a credential to ask with.
    daemon.apiText.mockRejectedValue(new ApiUnavailable("token", "the daemon token could not be read"));

    renderWithQuery(<ConnectionGate>{shell()}</ConnectionGate>);

    const text = await takeoverText();
    expect(text).toMatch(/not authorised/i);
    expect(text).toMatch(/token/i);
    // The failure that this test exists to prevent: telling someone to wait for
    // something that will never happen.
    expect(text).not.toMatch(/retrying/i);
    expect(screen.queryByText("the shell")).toBeNull();
  });

  it("reads a 401 as not authorised rather than as an outage", async () => {
    daemon.probeHealth.mockResolvedValue(true);
    daemon.apiText.mockRejectedValue(new ApiRefusal(401, "unauthorized", ""));

    renderWithQuery(<ConnectionGate>{shell()}</ConnectionGate>);

    expect(await takeoverText()).toMatch(/not authorised/i);
  });

  it("produces two different takeovers, and neither of them is a generic failure", async () => {
    daemon.probeHealth.mockResolvedValue(false);
    const first = renderWithQuery(<ConnectionGate>{shell()}</ConnectionGate>);
    const unreachable = await takeoverText();
    first.unmount();

    daemon.probeHealth.mockResolvedValue(true);
    daemon.apiText.mockRejectedValue(new ApiUnavailable("token", "no token"));
    renderWithQuery(<ConnectionGate>{shell()}</ConnectionGate>);
    const unauthorised = await takeoverText();

    expect(unreachable).not.toBe(unauthorised);
    for (const text of [unreachable, unauthorised]) {
      expect(text).not.toMatch(/request failed/i);
      expect(text).not.toMatch(/something went wrong/i);
    }
  });

  it("lets the shell through once the handshake completes", async () => {
    daemon.probeHealth.mockResolvedValue(true);
    daemon.apiText.mockResolvedValue("daemon running");

    renderWithQuery(<ConnectionGate>{shell()}</ConnectionGate>);

    expect(await screen.findByText("the shell")).toBeDefined();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("does not take the window over for a bad route", async () => {
    daemon.probeHealth.mockResolvedValue(true);
    // The daemon is up and authorising; `/status` itself broke. Most of this app
    // does not need `/status`, and a takeover would cost every page over one.
    daemon.apiText.mockRejectedValue(new ApiRefusal(500, "internal", "boom"));

    renderWithQuery(<ConnectionGate>{shell()}</ConnectionGate>);

    expect(await screen.findByText("the shell")).toBeDefined();
  });

  it("says it is still looking rather than reporting an outage it has not measured", async () => {
    // A probe that never settles: the first read is in flight, which is not the
    // same as a daemon that is down. A gate that showed "unreachable" for the
    // first moments of every cold start teaches people to ignore it.
    daemon.probeHealth.mockReturnValue(new Promise<boolean>(() => {}));

    renderWithQuery(<ConnectionGate>{shell()}</ConnectionGate>);

    expect(screen.getByText(/reaching the núcleo/)).toBeDefined();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText("the shell")).toBeNull();
  });
});
