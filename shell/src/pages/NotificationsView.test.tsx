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
import type { NotifyPolicy } from "../data/feed";
import { renderWithQuery } from "../test/harness";

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

    // `team_` is the case a screen built on FEED_KINDS could not show at all —
    // that table has no `team_` kind — and `council` is the empty family that is
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
    expect((within(jobs).getByRole("checkbox") as HTMLInputElement).checked).toBe(false);
    const team = screen.getByText("team").closest("label")!;
    expect((within(team).getByRole("checkbox") as HTMLInputElement).checked).toBe(true);
  });

  it("sends the whole policy, with exactly one rule per family switch", async () => {
    const sent: NotifyPolicy[] = [];
    serve({ families: [], kinds: [] }, OBSERVED, (body) => {
      sent.push(body);
      return undefined;
    });
    renderWithQuery(<NotificationsView />);

    const jobs = (await screen.findByText("jobs")).closest("label")!;
    fireEvent.click(within(jobs).getByRole("checkbox"));
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
    fireEvent.click(within(jobs).getByRole("checkbox"));
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
    const box = within(jobs).getByRole("checkbox");
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
    const box = within(jobs).getByRole("checkbox");
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
    const loose = await screen.findByLabelText("config_written");
    expect(loose.tagName).toBe("INPUT");
    expect((loose as HTMLInputElement).checked).toBe(true);

    // A kind inside a family keeps the three states, because it has one to
    // inherit from.
    fireEvent.click(screen.getByRole("button", { name: /2 kinds/ }));
    expect(screen.getByLabelText("job_failed").tagName).toBe("SELECT");
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
    fireEvent.click(within(jobs).getByRole("checkbox"));
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
});
