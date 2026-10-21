import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { NotificationsView } from "./NotificationsView";
import { ApiRefusal } from "../data/client";
import { mergePolicy, type ChannelNotifyPolicy, type NotifyPolicy } from "../data/feed";
import { QueryClientProvider } from "@tanstack/react-query";
import type { ReactElement } from "react";
import { renderWithQuery } from "../test/harness";

/** Swap the rendered tree while keeping the same query client (the harness wraps it once). */
function rerenderWith(view: ReturnType<typeof renderWithQuery>, ui: ReactElement) {
  view.rerender(<QueryClientProvider client={view.queryClient}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
});

/** A daemon serving one policy and one kind list, recording what it is sent. */
function serve(policy: NotifyPolicy, kinds: string[], onPut?: (body: NotifyPolicy) => unknown) {
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    if (path === "/notifications/policy" && (init?.method ?? "GET") === "GET") return policy;
    if (path === "/notifications/kinds") return kinds;
    if (path === "/notifications/policy" && init?.method === "PUT") {
      const body = JSON.parse(init.body as string) as NotifyPolicy;
      if (onPut) return onPut(body);
      policy = body;
      return undefined;
    }
    throw new Error(`unexpected ${init?.method ?? "GET"} ${path}`);
  });
}

const OBSERVED = ["job_started", "job_failed", "team_message", "config_written"];

describe("the notifications view", () => {
  it("draws every family, including one this machine has written no kinds for", async () => {
    serve({ families: [], kinds: [] }, OBSERVED);
    renderWithQuery(<NotificationsView />);

    // `team_` is the family a screen built on a table of kinds would fall behind
    // on, and `council` is the empty family that is
    // shown rather than hidden.
    expect(await screen.findByText("jobs")).toBeDefined();
    expect(screen.getByText("team")).toBeDefined();
    expect(screen.getByText("council")).toBeDefined();
  });

  it("says in writing what these switches cannot silence", async () => {
    serve({ families: [], kinds: [] }, OBSERVED);
    renderWithQuery(<NotificationsView />);

    // Both sentences are on screen rather than discovered by experiment: that
    // governance is never silenced, and that nothing needs restarting.
    expect(await screen.findByText(/never silenced/i)).toBeDefined();
    expect(screen.getByText(/nothing needs restarting/i)).toBeDefined();
  });

  it("draws a switch with no stored rule as on, because that is what it does", async () => {
    serve({ families: [{ selector: "job_", enabled: false }], kinds: [] }, OBSERVED);
    renderWithQuery(<NotificationsView />);

    const jobs = (await screen.findByText("jobs")).closest("label")!;
    expect((within(jobs).getByRole("switch") as HTMLInputElement).checked).toBe(false);
    const team = screen.getByText("team").closest("label")!;
    expect((within(team).getByRole("switch") as HTMLInputElement).checked).toBe(true);
  });

  it("sends the whole policy, with exactly one rule per family switch", async () => {
    const sent: NotifyPolicy[] = [];
    serve({ families: [], kinds: [] }, OBSERVED, (body) => {
      sent.push(body);
      return undefined;
    });
    renderWithQuery(<NotificationsView />);

    const jobs = (await screen.findByText("jobs")).closest("label")!;
    fireEvent.click(within(jobs).getByRole("switch"));
    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    await waitFor(() => expect(sent).toHaveLength(1));
    // One family switch, one rule — the whole point of one family being exactly
    // one prefix. And turning it off stores `false` rather than removing it.
    expect(sent[0]).toEqual({ families: [{ selector: "job_", enabled: false }], kinds: [] });
  });

  it("removes a family's rule when it is turned back on, rather than storing true", async () => {
    const sent: NotifyPolicy[] = [];
    serve({ families: [{ selector: "job_", enabled: false }], kinds: [] }, OBSERVED, (body) => {
      sent.push(body);
      return undefined;
    });
    renderWithQuery(<NotificationsView />);

    const jobs = (await screen.findByText("jobs")).closest("label")!;
    fireEvent.click(within(jobs).getByRole("switch"));
    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    // Absence and "allowed" behave the same today, but only absence keeps
    // behaving like a machine nobody has configured.
    await waitFor(() => expect(sent).toHaveLength(1));
    expect(sent[0]).toEqual({ families: [], kinds: [] });
  });

  it("has nothing to save after a switch is flipped and flipped back", async () => {
    serve({ families: [], kinds: [] }, OBSERVED);
    renderWithQuery(<NotificationsView />);

    const jobs = (await screen.findByText("jobs")).closest("label")!;
    const box = within(jobs).getByRole("switch");
    fireEvent.click(box);
    expect(screen.queryByText(/unsaved changes/i)).not.toBeNull();
    fireEvent.click(box);

    // Editing a rule removes and re-appends it, so the array comes back in a
    // different ORDER while saying the same thing. Comparing the two as JSON
    // would leave the button lit over nothing to save.
    expect(screen.queryByText(/unsaved changes/i)).toBeNull();
    expect((screen.getByRole("button", { name: /save/i }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("keeps an explicit `enabled: true` family rule through a flip and flip back", async () => {
    // A stored `true` is inert — absence behaves the same — but it is a decision
    // somebody wrote down ("the jobs stay on"), and spec §5.3 keeps it. Mapping
    // "on" to rule-removal would delete it on the way past, and the screen would
    // offer to save that deletion.
    serve({ families: [{ selector: "job_", enabled: true }], kinds: [] }, OBSERVED);
    renderWithQuery(<NotificationsView />);

    const jobs = (await screen.findByText("jobs")).closest("label")!;
    const box = within(jobs).getByRole("switch");
    fireEvent.click(box);
    fireEvent.click(box);

    expect(screen.queryByText(/unsaved changes/i)).toBeNull();
  });

  it("keeps a stored rule visible after its kind falls out of the 90-day window", async () => {
    serve({ families: [], kinds: [{ selector: "job_gc_failed", enabled: false }] }, OBSERVED);
    renderWithQuery(<NotificationsView />);

    fireEvent.click(await screen.findByRole("button", { name: /3 kinds/ }));

    // The row exists at all — without the union it would silence with nowhere
    // to be undone — and it says why it is unfamiliar.
    expect((screen.getByLabelText("job_gc_failed") as HTMLSelectElement).value).toBe("never");
    expect(screen.getByText(/not seen in the last 90 days/i)).toBeDefined();
  });

  it("gives a loose kind a plain switch, not a family to follow", async () => {
    serve({ families: [], kinds: [] }, OBSERVED);
    renderWithQuery(<NotificationsView />);

    // `config_written` is claimed by no prefix, so "follows its family" would
    // name something that does not exist for it.
    const loose = await screen.findByLabelText("project file written");
    expect(loose.tagName).toBe("INPUT");
    expect((loose as HTMLInputElement).checked).toBe(true);

    // A kind inside a family keeps the three states, because it has one to
    // inherit from.
    fireEvent.click(screen.getByRole("button", { name: /2 kinds/ }));
    expect(screen.getByLabelText("job failed").tagName).toBe("SELECT");
  });

  it("shows the selector the núcleo objected to, not a generic failure", async () => {
    // Built the way `refusalFrom` builds one from the núcleo's body: the code, and
    // the sentence out of `detail`. That the WIRE carries the sentence under
    // `detail` and not `message` is the núcleo's to pin, and
    // `a_policy_put_replaces_and_a_refusal_writes_nothing` does; this is the other
    // half — that the sentence, once it arrives, reaches the screen.
    //
    // Asserting the SELECTOR and not just the code is the point. `RefusalNote`
    // prints the code unconditionally, so a test that looked only for
    // `malformed_selector` would pass with the detail thrown away — which is
    // exactly the generic message spec §7.4 refuses.
    serve({ families: [], kinds: [] }, OBSERVED, () => {
      throw new ApiRefusal(
        400,
        "malformed_selector",
        'the kind selector "Needs Reply" is not lowercase letters, digits, `_` or `.`',
      );
    });
    renderWithQuery(<NotificationsView />);

    const jobs = (await screen.findByText("jobs")).closest("label")!;
    fireEvent.click(within(jobs).getByRole("switch"));
    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    expect(await screen.findByText(/Needs Reply/)).toBeDefined();
    expect(screen.getByText("malformed_selector")).toBeDefined();
  });

  it("says nothing is known when the policy cannot be read", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/notifications/kinds") return OBSERVED;
      throw new Error("the daemon did not answer");
    });
    renderWithQuery(<NotificationsView />);

    expect(await screen.findByText(/nothing is known about the notification policy/i)).toBeDefined();
  });

  describe("the channel scope selector", () => {
    const channel = (id: string, label: string, configured: boolean) => ({
      id,
      kind: id,
      label,
      health_row: id,
      configured,
      missing_secrets: [],
      settings_area: id,
      state: configured ? "online" : "not_configured",
      since: null,
      adapter: null,
      capabilities: null,
      home: null,
      identities: { senders: 0, conversations: 0 },
      pairing: null,
      outbox: { pending: 0, leased: 0, failed: 0, oldest_due_at: null },
      drops: { unknown_sender: 0, pairing_failures: 0, counted_since: "2026-10-19T00:00:00Z" },
    });
    const READOUT = {
      contract_version: "1",
      primary: "telegram",
      routing: { state: "ok" },
      channels: [
        channel("telegram", "Telegram", true),
        channel("email", "Email", true),
        channel("matrix", "Matrix", false),
      ],
    };

    /** `serve`, plus `GET /channels` answered by `channels` (a function, so a test can fail or stall it). */
    function serveWithChannels(channels: () => unknown) {
      serve({ families: [], kinds: [] }, OBSERVED);
      const base = daemon.apiFetch.getMockImplementation()!;
      daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
        if (path === "/channels") return channels();
        // Channel mode (chunk 10) reads the channel's own policy; an empty one is enough here.
        if (path.startsWith("/notifications/policy?channel=")) {
          const empty = { families: [], kinds: [] };
          return { channel: "telegram", primary: true, default: empty, overrides: empty, resolved: empty };
        }
        return base(path, init);
      });
    }

    it("titles the panel neutrally", async () => {
      serve({ families: [], kinds: [] }, OBSERVED);
      renderWithQuery(<NotificationsView />);
      expect(await screen.findByText("Notifications")).toBeDefined();
      expect(screen.queryByText("Telegram notifications")).toBeNull();
    });

    it("draws no selector without onChannelChange", async () => {
      serve({ families: [], kinds: [] }, OBSERVED);
      renderWithQuery(<NotificationsView />);
      await screen.findByText("jobs");
      expect(screen.queryByLabelText(/channel/i)).toBeNull();
    });

    it("offers Default and the configured channels, marking the primary", async () => {
      serveWithChannels(() => READOUT);
      renderWithQuery(<NotificationsView onChannelChange={vi.fn()} />);

      await screen.findByText("jobs");
      await waitFor(() =>
        expect((screen.getByLabelText(/channel/i) as HTMLSelectElement).disabled).toBe(false),
      );
      const select = screen.getByLabelText(/channel/i) as HTMLSelectElement;
      const names = Array.from(select.options).map((o) => o.textContent ?? "");
      expect(names).toHaveLength(3);
      expect(names[0]).toBe("Default (all channels)");
      expect(names[1]).toMatch(/Telegram/);
      expect(names[1]).toMatch(/primary/);
      expect(names[2]).toBe("Email");
      expect(names.join(" ")).not.toMatch(/Matrix/);
    });

    it("reports the choice, and Default as undefined", async () => {
      serveWithChannels(() => READOUT);
      const onChannelChange = vi.fn();
      renderWithQuery(<NotificationsView channel="telegram" onChannelChange={onChannelChange} />);

      await screen.findByText("jobs");
      await waitFor(() =>
        expect((screen.getByLabelText(/channel/i) as HTMLSelectElement).disabled).toBe(false),
      );
      const select = screen.getByLabelText(/channel/i) as HTMLSelectElement;
      expect(select.value).toBe("telegram");

      fireEvent.change(select, { target: { value: "email" } });
      expect(onChannelChange).toHaveBeenLastCalledWith("email");
      fireEvent.change(select, { target: { value: "" } });
      expect(onChannelChange).toHaveBeenLastCalledWith(undefined);
    });

    it("keeps Default working, with a quiet note, when the channel list fails", async () => {
      serveWithChannels(() => {
        throw new Error("the daemon did not answer");
      });
      renderWithQuery(<NotificationsView onChannelChange={vi.fn()} />);

      expect(await screen.findByText("jobs")).toBeDefined();
      expect(await screen.findByText(/channel list did not load/i)).toBeDefined();
      const select = screen.getByLabelText(/channel/i) as HTMLSelectElement;
      expect(Array.from(select.options).map((o) => o.textContent)).toEqual(["Default (all channels)"]);
    });

    it("disables the selector while the channels load", async () => {
      serveWithChannels(() => new Promise(() => {}));
      renderWithQuery(<NotificationsView onChannelChange={vi.fn()} />);

      await screen.findByText("jobs");
      expect((screen.getByLabelText(/channel/i) as HTMLSelectElement).disabled).toBe(true);
    });
  });

  describe("channel mode", () => {
    const rule = (selector: string, enabled: boolean) => ({ selector, enabled });
    const DEFAULTS: NotifyPolicy = {
      families: [rule("worktree_", false), rule("job_", true)],
      kinds: [],
    };
    const policyFor = (
      channel: string,
      overrides: NotifyPolicy,
      primary = true,
    ): ChannelNotifyPolicy => ({
      channel,
      primary,
      default: DEFAULTS,
      overrides,
      resolved: mergePolicy(DEFAULTS, overrides),
    });

    /** A daemon serving per-channel policies (and the default one), recording every PUT. */
    function serveChannels(
      byChannel: Record<string, ChannelNotifyPolicy>,
      puts: { path: string; body: unknown }[] = [],
    ) {
      daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
        const method = init?.method ?? "GET";
        if (path === "/notifications/kinds") return OBSERVED;
        if (path === "/notifications/policy" && method === "GET") return { families: [], kinds: [] };
        if (path === "/notifications/policy" && method === "PUT") {
          puts.push({ path, body: JSON.parse(init!.body as string) });
          return undefined;
        }
        const match = /^\/notifications\/policy\?channel=(.+)$/.exec(path);
        if (match) {
          const id = decodeURIComponent(match[1]);
          if (method === "PUT") {
            puts.push({ path, body: JSON.parse(init!.body as string) });
            return undefined;
          }
          if (id === "ghost") throw new ApiRefusal(404, "unknown_channel", "no channel called ghost");
          return byChannel[id];
        }
        if (path === "/channels") return { contract_version: "1", primary: "telegram", routing: { state: "ok" }, channels: [] };
        throw new Error(`unexpected ${method} ${path}`);
      });
    }

    const familyRow = async (name: string) => (await screen.findByText(name)).closest("li")!;

    it("draws the overrides laid over the default, with a source mark per row", async () => {
      serveChannels({
        telegram: policyFor("telegram", { families: [rule("worktree_", true), rule("job_", true)], kinds: [] }),
      });
      renderWithQuery(<NotificationsView channel="telegram" onChannelChange={vi.fn()} />);

      const worktrees = await familyRow("worktrees");
      // The override `true` beats the default `false`.
      expect((within(worktrees).getByRole("switch") as HTMLInputElement).checked).toBe(true);
      expect(within(worktrees).getByText("override")).toBeDefined();
      // Same value as the default: pinned to it.
      const jobs = await familyRow("jobs");
      expect(within(jobs).getByText("pinned")).toBeDefined();
      expect(within(jobs).getByTitle(/pinned to the default/i)).toBeDefined();
      // No override at all: inherits.
      const team = await familyRow("team");
      expect(within(team).getByText("default")).toBeDefined();
      expect(within(team).queryByRole("button", { name: /reset to default/i })).toBeNull();
    });

    it("resets an overridden row back to the default", async () => {
      serveChannels({
        telegram: policyFor("telegram", { families: [rule("worktree_", true)], kinds: [] }),
      });
      renderWithQuery(<NotificationsView channel="telegram" onChannelChange={vi.fn()} />);

      const worktrees = await familyRow("worktrees");
      fireEvent.click(within(worktrees).getByRole("button", { name: /reset to default/i }));

      const after = await familyRow("worktrees");
      expect(within(after).getByText("default")).toBeDefined();
      expect((within(after).getByRole("switch") as HTMLInputElement).checked).toBe(false);
      expect(within(after).queryByRole("button", { name: /reset to default/i })).toBeNull();
      expect(screen.queryByText(/unsaved changes/i)).not.toBeNull();
    });

    it("saves only the overrides, to the channel's URL, and only when dirty", async () => {
      const puts: { path: string; body: unknown }[] = [];
      serveChannels({ telegram: policyFor("telegram", { families: [], kinds: [] }) }, puts);
      renderWithQuery(<NotificationsView channel="telegram" onChannelChange={vi.fn()} />);

      const save = (await screen.findByRole("button", { name: /^save$/i })) as HTMLButtonElement;
      expect(save.disabled).toBe(true);

      // Turning the (default-off) worktrees family on must store an explicit `true`.
      const worktrees = await familyRow("worktrees");
      fireEvent.click(within(worktrees).getByRole("switch"));
      expect(save.disabled).toBe(false);
      fireEvent.click(save);

      await waitFor(() => expect(puts).toHaveLength(1));
      expect(puts[0].path).toBe("/notifications/policy?channel=telegram");
      expect(puts[0].body).toEqual({ families: [rule("worktree_", true)], kinds: [] });
      expect(Object.keys(puts[0].body as object).sort()).toEqual(["families", "kinds"]);
    });

    it("notes that a channel which is not the primary is dormant", async () => {
      serveChannels({ email: policyFor("email", { families: [], kinds: [] }, false) });
      renderWithQuery(<NotificationsView channel="email" onChannelChange={vi.fn()} />);

      expect(await screen.findByText(/dormant/i)).toBeDefined();
      expect(screen.getByText(/when it becomes the primary/i)).toBeDefined();
    });

    it("has no dormant note on the primary channel", async () => {
      serveChannels({ telegram: policyFor("telegram", { families: [], kinds: [] }) });
      renderWithQuery(<NotificationsView channel="telegram" onChannelChange={vi.fn()} />);

      await familyRow("jobs");
      expect(screen.queryByText(/dormant/i)).toBeNull();
    });

    it("says an empty override set inherits everything, and stops once there is one", async () => {
      serveChannels({
        telegram: policyFor("telegram", { families: [], kinds: [] }),
        email: policyFor("email", { families: [rule("team_", false)], kinds: [] }),
      });
      const view = renderWithQuery(<NotificationsView channel="telegram" onChannelChange={vi.fn()} />);
      expect(await screen.findByText(/inherits everything from the default/i)).toBeDefined();

      rerenderWith(view, <NotificationsView channel="email" onChannelChange={vi.fn()} />);
      await waitFor(() => expect(screen.queryByText(/inherits everything/i)).toBeNull());
    });

    it("shows a refusal of the PUT", async () => {
      serveChannels({ telegram: policyFor("telegram", { families: [], kinds: [] }) });
      const base = daemon.apiFetch.getMockImplementation()!;
      daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
        if (init?.method === "PUT") throw new ApiRefusal(400, "selector_too_long", "selector is too long");
        return base(path, init);
      });
      renderWithQuery(<NotificationsView channel="telegram" onChannelChange={vi.fn()} />);

      const worktrees = await familyRow("worktrees");
      fireEvent.click(within(worktrees).getByRole("switch"));
      fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

      expect(await screen.findByText("selector_too_long")).toBeDefined();
    });

    it("shows a refusal of the GET instead of loading forever", async () => {
      serveChannels({});
      renderWithQuery(<NotificationsView channel="ghost" onChannelChange={vi.fn()} />);

      expect(await screen.findByText("unknown_channel")).toBeDefined();
      expect(screen.queryByText(/reading the notification policy/i)).toBeNull();
    });

    it("re-seeds the draft when the channel changes, without mixing drafts", async () => {
      serveChannels({
        telegram: policyFor("telegram", { families: [], kinds: [] }),
        email: policyFor("email", { families: [rule("team_", false)], kinds: [] }),
      });
      const view = renderWithQuery(<NotificationsView channel="telegram" onChannelChange={vi.fn()} />);

      const worktrees = await familyRow("worktrees");
      fireEvent.click(within(worktrees).getByRole("switch"));
      expect(screen.queryByText(/unsaved changes/i)).not.toBeNull();

      rerenderWith(view, <NotificationsView channel="email" onChannelChange={vi.fn()} />);
      await waitFor(() => expect(screen.queryByText(/unsaved changes/i)).toBeNull());
      const worktreesAfter = await familyRow("worktrees");
      expect((within(worktreesAfter).getByRole("switch") as HTMLInputElement).checked).toBe(false);
      const team = await familyRow("team");
      expect(within(team).getByText("override")).toBeDefined();
    });

    it("goes back to the old behaviour on Default, without asking for a channel", async () => {
      serveChannels({ telegram: policyFor("telegram", { families: [], kinds: [] }) });
      const view = renderWithQuery(<NotificationsView channel="telegram" onChannelChange={vi.fn()} />);
      await familyRow("jobs");

      daemon.apiFetch.mockClear();
      rerenderWith(view, <NotificationsView onChannelChange={vi.fn()} />);
      await screen.findByText("jobs");
      expect(daemon.apiFetch.mock.calls.map((c) => c[0])).not.toContain(
        "/notifications/policy?channel=telegram",
      );
    });

    it("leaves Default mode unchanged when a handler is passed but no channel", async () => {
      const puts: { path: string; body: unknown }[] = [];
      serveChannels({}, puts);
      renderWithQuery(<NotificationsView onChannelChange={vi.fn()} />);

      const jobs = await familyRow("jobs");
      expect(daemon.apiFetch.mock.calls.map((c) => c[0])).toContain("/notifications/policy");
      expect(
        daemon.apiFetch.mock.calls.some((c) => String(c[0]).includes("?channel=")),
      ).toBe(false);
      expect(screen.queryByText(/reset to default/i)).toBeNull();
      expect(within(jobs).queryByText(/^(default|override|pinned)$/)).toBeNull();

      fireEvent.click(within(jobs).getByRole("switch"));
      fireEvent.click(screen.getByRole("button", { name: /^save$/i }));
      await waitFor(() => expect(puts).toHaveLength(1));
      expect(puts[0].path).toBe("/notifications/policy");
      expect(puts[0].body).toEqual({ families: [rule("job_", false)], kinds: [] });
    });
  });

  it("the desktop switch remembers its state", async () => {
    localStorage.clear();
    serve({ families: [], kinds: [] }, OBSERVED);
    const first = renderWithQuery(<NotificationsView />);

    // On by default: a machine nobody has configured shows toasts, as the owner decided.
    const label = /also show these on this computer's desktop/i;
    const box = (await screen.findByLabelText(label)) as HTMLInputElement;
    expect(box.checked).toBe(true);

    fireEvent.click(box);
    expect(box.checked).toBe(false);
    expect(localStorage.getItem("nucleos.desktopNotify")).toBe("false");

    // A fresh mount reads it back; it is not a draft that waits for Save.
    first.unmount();
    renderWithQuery(<NotificationsView />);
    expect(((await screen.findByLabelText(label)) as HTMLInputElement).checked).toBe(false);
    localStorage.clear();
  });
});
