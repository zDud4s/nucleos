import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import App from "./App";
import { invoke } from "@tauri-apps/api/core";

// The credential-manager read is the one thing here that is not HTTP.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);
const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

/** A daemon that is up, healthy, and happy with the token. */
function healthyDaemon(overrides: Record<string, unknown> = {}) {
  return async (url: string) => {
    for (const [suffix, response] of Object.entries(overrides)) {
      if (url.endsWith(suffix)) return response;
    }
    if (url.endsWith("/health")) return { ok: true, status: 200, text: async () => "ok" };
    if (url.endsWith("/status")) return { ok: true, status: 200, text: async () => "idle" };
    if (url.endsWith("/autopilot/kill")) return { ok: true, status: 200, json: async () => ({ engaged: false }) };
    if (url.endsWith("/autopilot/budget")) {
      return {
        ok: true,
        status: 200,
        json: async () => ({
          limit_usd: null, period: "daily", hourly_limit_usd: null,
          per_run_reserve_usd: 0.5, time_cost_per_hour_usd: 0,
          window_spend_usd: 0, hourly_spend_usd: 0, paused: false, reason: null,
        }),
      };
    }
    return { ok: true, status: 200, json: async () => [] };
  };
}

/** Advances the poll clock and lets every promise it started settle. */
async function tick(ms = 3000) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
  });
}

async function settle() {
  await act(async () => {});
}

describe("App connection handshake", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockImplementation(healthyDaemon());
    invokeMock.mockResolvedValue("daemon-token");
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
    invokeMock.mockReset();
  });

  it("retries a failed credential read instead of remembering the failure forever", async () => {
    // The keychain is commonly locked for a moment right after login. Caching
    // the promise made that one failure permanent for the whole session.
    invokeMock
      .mockRejectedValueOnce("No matching entry found in secure storage")
      .mockResolvedValue("daemon-token");

    render(<App />);
    await settle();

    expect(screen.getByText(/No matching entry found in secure storage/)).toBeTruthy();
    expect(screen.queryByText("daemon connected")).toBeNull();

    await tick();

    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(screen.getByText("daemon connected")).toBeTruthy();
    expect(screen.queryByText(/No matching entry found/)).toBeNull();
  });

  it("says the daemon refused the token rather than pretending it is connected", async () => {
    fetchMock.mockImplementation(
      healthyDaemon({ "/status": { ok: false, status: 401, text: async () => "" } }),
    );

    render(<App />);
    await settle();

    // Reachable but rejected: retrying forever cannot fix a stale token, so
    // the header must not keep claiming everything is fine.
    expect(screen.queryByText("daemon connected")).toBeNull();
    expect(screen.getByText(/rejected the stored token/)).toBeTruthy();
  });

  it("recovers once the daemon accepts the token again", async () => {
    let authorised = false;
    fetchMock.mockImplementation(async (url: string) => {
      if (url.endsWith("/status")) {
        return authorised
          ? { ok: true, status: 200, text: async () => "idle" }
          : { ok: false, status: 401, text: async () => "" };
      }
      return healthyDaemon()(url);
    });

    render(<App />);
    await settle();
    expect(screen.getByText(/rejected the stored token/)).toBeTruthy();

    authorised = true;
    await tick();

    expect(screen.getByText("daemon connected")).toBeTruthy();
    // A refusal drops the cached token so the next round re-reads the
    // credential manager: the daemon may have rotated it.
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("never stacks a second poll on top of one still in flight", async () => {
    let releaseHealth: ((value: unknown) => void) | null = null;
    fetchMock.mockImplementation(async (url: string) => {
      if (url.endsWith("/health")) {
        return new Promise((resolve) => {
          releaseHealth = () => resolve({ ok: true, status: 200, text: async () => "ok" });
        });
      }
      return healthyDaemon()(url);
    });

    render(<App />);
    await settle();
    await tick(9000);

    // Three ticks passed with the first request unanswered; a slow daemon must
    // not accumulate rounds whose answers then land out of order.
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      releaseHealth?.(undefined);
    });
    await tick();
    expect(fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/health")).length).toBe(2);
  });
});

describe("App navigation and presence", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockImplementation(healthyDaemon());
    invokeMock.mockResolvedValue("daemon-token");
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
    invokeMock.mockReset();
  });

  function heartbeats() {
    return fetchMock.mock.calls.filter(([url]) =>
      String(url).endsWith("/autopilot/attention"),
    ).length;
  }

  /** jsdom reports `visible`; this is how a minimised or backgrounded window is simulated. */
  function setVisibility(state: "visible" | "hidden") {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => state,
    });
    document.dispatchEvent(new Event("visibilitychange"));
  }

  afterEach(() => setVisibility("visible"));

  it("offers every view the daemon has a page for", async () => {
    render(<App />);
    await settle();

    const nav = screen.getByLabelText("NucleOS views");
    // The ORDER is asserted, not just the membership, and Fleet's place in it is load-bearing: the
    // `approvals` comment in `App.tsx` says Waiting comes "straight after Autopilot", so anything
    // inserted between those two would make that sentence false.
    expect(
      Array.from(nav.querySelectorAll("button")).map((button) => button.textContent),
    ).toEqual([
      "Home", "Fleet", "Autopilot", "Waiting", "Runs", "Projects", "Chats",
      // Browser sits beside Web because the two are constantly mistaken for each other: Web is what
      // has been READ — one fetch, no session, no cookies — and Browser is what has been BROWSED, in
      // a profile that holds the owner's logins. Then Agents and Teams, in that order and still
      // immediately before Council: who exists, then who works together, then the tab that spends
      // on purpose. Three tabs landed here from three branches and every neighbourhood App.tsx
      // claims in prose survives all of them.
      "Mail", "Files", "Contacts", "Voice", "Calendar", "Web", "Browser", "Agents", "Teams",
      "Council", "System",
    ]);
  });

  it("tells the daemon someone is watching, and keeps saying so", async () => {
    render(<App />);
    await settle();

    // Immediately on connecting, not at the next tick: the brake should arm as soon as the shell
    // is in front of someone.
    expect(heartbeats()).toBe(1);

    // The daemon's window is 120s, so a 30s cadence survives a couple of missed beats.
    await tick(30000);
    expect(heartbeats()).toBe(2);
  });

  it("stops claiming presence while the window is hidden", async () => {
    render(<App />);
    await settle();
    expect(heartbeats()).toBe(1);

    setVisibility("hidden");
    await tick(60000);

    // A minimised shell is not a foreground client, so the heartbeat lapses and autonomous work is
    // free to start again — which is the whole point of it expiring.
    expect(heartbeats()).toBe(1);

    setVisibility("visible");
    await settle();

    // Coming back registers at once rather than waiting out the rest of the interval.
    expect(heartbeats()).toBe(2);
  });
});

/**
 * The assistant's transcript belongs to App, not to the page that draws it.
 *
 * Tabs render one page at a time, so leaving the assistant unmounts it. With the transcript in the
 * page's own state, the message you had just sent vanished on the way out — and the poll waiting for
 * its answer died with it, so the turn could never finish even after the daemon had answered.
 */
describe("the assistant transcript survives a tab switch", () => {
  /** The one conversation these tests talk in. */
  const THE_CHAT = {
    chat_id: "c1",
    title: "the one",
    brain: "cloud" as const,
    created_at: "2026-08-11T10:00:00+00:00",
    first_message: null,
    last_activity: null,
    waiting: 0,
  };

  /** A daemon that takes a message as turn 501 and reports whatever `status` currently says. */
  function assistantDaemon(status: () => { status: string; stdout: string | null }) {
    return async (url: string) => {
      if (url.endsWith("/assistant/message")) {
        return { ok: true, status: 200, json: async () => ({ turn_id: 501 }) };
      }
      if (url.endsWith("/assistant/local-model")) {
        return { ok: true, status: 200, json: async () => ({ available: false }) };
      }
      // Checked before the bare `/assistant/chats`, which is a suffix of this one.
      if (url.includes("/assistant/chats/")) {
        return { ok: true, status: 200, json: async () => [] };
      }
      if (url.endsWith("/assistant/chats")) {
        return { ok: true, status: 200, json: async () => [THE_CHAT] };
      }
      if (url.endsWith("/assistant/501")) {
        const now = status();
        return {
          ok: true,
          status: 200,
          json: async () => ({
            id: 501, project_id: null, status: now.status,
            gate_status: null, gate_exit_code: null, gate_output: null,
            exit_code: 0, stdout: now.stdout, stderr: null, session_id: null,
            cost_usd: 0.02, input_tokens: 10, output_tokens: 5,
            cache_read_tokens: 0, num_turns: 1,
          }),
        };
      }
      return healthyDaemon()(url);
    };
  }

  function go(tab: string) {
    fireEvent.click(screen.getByRole("button", { name: tab }));
  }

  /** Enters the chats and opens the one conversation the daemon above is holding. */
  async function openTheChat() {
    go("Chats");
    await settle();
    fireEvent.click(screen.getByText("the one"));
    await settle();
  }

  /** The one way out of the chats: inside them the tab strip stands down. */
  async function back() {
    fireEvent.click(screen.getByRole("button", { name: "← Back" }));
    await settle();
  }

  async function ask(what: string) {
    fireEvent.change(screen.getByPlaceholderText("Ask the núcleo…"), { target: { value: what } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await settle();
  }

  beforeEach(() => {
    vi.useFakeTimers();
    invokeMock.mockResolvedValue("daemon-token");
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
    invokeMock.mockReset();
  });

  /**
   * The thread is the núcleo's, not this window's.
   *
   * Every turn is a run and the run row now records which chat it belonged to, so a conversation is
   * read back rather than remembered — which is what makes it survive a restart, not merely a tab
   * switch. `/runs?mode=assistant` could never have stood in for it: that is every chat at once,
   * the Telegram sidecar's turns included.
   */
  it("loads the conversation from the daemon rather than starting empty", async () => {
    fetchMock.mockImplementation(async (url: string) => {
      if (String(url).endsWith("/assistant/local-model")) {
        return { ok: true, status: 200, json: async () => ({ available: false }) };
      }
      if (String(url).includes("/assistant/chats/")) {
        return {
          ok: true,
          status: 200,
          json: async () => [
            { id: 41, asked: "what did I ask before?", answer: "this", error: null,
              status: "completed", cost_usd: 0.01, answered_by: "cloud",
              created_at: "2026-07-30T10:00:00Z" },
          ],
        };
      }
      if (String(url).endsWith("/assistant/chats")) {
        return { ok: true, status: 200, json: async () => [THE_CHAT] };
      }
      return healthyDaemon()(url);
    });

    render(<App />);
    await settle();
    await openTheChat();

    expect(screen.getByText("what did I ask before?")).toBeTruthy();
    expect(screen.getByText("this")).toBeTruthy();
  });

  /// The number stands next to a door, so it counts places to go, not things to read.
  it("counts conversations on the tab, not answers", async () => {
    fetchMock.mockImplementation(async (url: string) => {
      if (String(url).endsWith("/assistant/chats")) {
        return {
          ok: true,
          status: 200,
          json: async () => [
            { ...THE_CHAT, chat_id: "a", waiting: 4 },
            { ...THE_CHAT, chat_id: "b", waiting: 1 },
            { ...THE_CHAT, chat_id: "c", waiting: 0 },
          ],
        };
      }
      return healthyDaemon()(url);
    });

    render(<App />);
    await settle();

    // Five answers across two conversations: two visits to make, which is the decision in hand.
    expect(screen.getByLabelText("2 conversations waiting")).toBeTruthy();
  });

  it("says nothing on the tab when nothing is waiting", async () => {
    // A badge reading "0" is something to look at that says nothing.
    fetchMock.mockImplementation(assistantDaemon(() => ({ status: "running", stdout: null })));

    render(<App />);
    await settle();

    expect(screen.queryByLabelText(/waiting/)).toBeNull();
  });

  it("hides the tab bar inside the chats and keeps the emergency stop", async () => {
    // "No tab bar" is the ask. "No emergency stop" is not — and the right side of the header is
    // where the kill switch lives.
    fetchMock.mockImplementation(assistantDaemon(() => ({ status: "running", stdout: null })));

    render(<App />);
    await settle();
    go("Chats");
    await settle();

    expect(screen.queryByLabelText("NucleOS views")).toBeNull();
    expect(screen.getByRole("button", { name: "Kill switch" })).toBeTruthy();
  });

  it("comes back to the bar through ← Back", async () => {
    fetchMock.mockImplementation(assistantDaemon(() => ({ status: "running", stdout: null })));

    render(<App />);
    await settle();
    go("Chats");
    await settle();
    await back();

    expect(screen.getByLabelText("NucleOS views")).toBeTruthy();
  });

  it("keeps a turn the daemon has not caught up with yet", async () => {
    // The row is inserted while the request that created it is still open, so a history read can
    // overtake it. Replacing wholesale would drop the message just sent — the very loss this is
    // meant to end — so the page keeps what only it knows about.
    fetchMock.mockImplementation(assistantDaemon(() => ({ status: "running", stdout: null })));

    render(<App />);
    await settle();
    await openTheChat();
    await ask("acabei de escrever isto");

    await back();
    go("Runs");
    await settle();
    go("Chats");
    await settle();

    expect(screen.getByText("acabei de escrever isto")).toBeTruthy();
  });

  it("still shows the message you just sent after leaving and coming back", async () => {
    fetchMock.mockImplementation(assistantDaemon(() => ({ status: "running", stdout: null })));

    render(<App />);
    await settle();
    await openTheChat();
    await ask("olá núcleo");

    expect(screen.getByText("olá núcleo")).toBeTruthy();

    await back();
    go("Runs");
    await settle();
    expect(screen.queryByText("olá núcleo")).toBeNull();

    go("Chats");
    await settle();

    // The whole point, twice over: the question is still on screen, and the conversation it belongs
    // to is still the open one — both are owned above the page that draws them.
    expect(screen.getByText("olá núcleo")).toBeTruthy();
  });

  it("collects an answer that landed while the tab was closed", async () => {
    let finished = false;
    fetchMock.mockImplementation(
      assistantDaemon(() =>
        finished
          ? { status: "completed", stdout: "olá de volta" }
          : { status: "running", stdout: null },
      ),
    );

    render(<App />);
    await settle();
    await openTheChat();
    await ask("estás aí?");

    await back();
    go("Runs");
    await settle();
    // The turn finishes in the daemon while nothing is watching it.
    finished = true;
    await tick(6000);

    go("Chats");
    await settle();
    // Remounting derives the pending turn back out of the transcript, so the poll restarts and
    // finds the answer. Held as separate state, this stayed on "Working…" forever.
    await tick(2000);

    expect(screen.getByText("olá de volta")).toBeTruthy();
  });
});
