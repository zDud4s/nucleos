import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Senders from "./Senders";
import type { Correspondent, MergeSide, MergeSuggestion } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function who(overrides: Partial<Correspondent> = {}): Correspondent {
  return {
    address: "maria@example.com", contact_id: 1, linked_by: "implicit",
    display_name: "Maria", messages_in: 12,
    outbound_ever: 0, first_seen: "2026-01-01T10:00:00Z", last_seen: "2026-07-29T10:00:00Z",
    verdict: null,
    ...overrides,
  };
}

function side(overrides: Partial<MergeSide> = {}): MergeSide {
  return {
    contact_id: 1, addresses: ["maria@example.com"], display_name: "Maria",
    messages_in: 12, verdict: null,
    ...overrides,
  };
}

function suggestion(overrides: Partial<MergeSuggestion> = {}): MergeSuggestion {
  return {
    proposal_id: 9, reasoning: "share a name", created_at: "2026-07-30T10:00:00Z",
    keep: side(), absorb: side({ contact_id: 2, addresses: ["m.silva@example.com"] }),
    ...overrides,
  };
}

function rosterOf(contacts: Correspondent[], merges: MergeSuggestion[] = []) {
  fetchMock.mockImplementation(async (url: string) => {
    if (String(url).includes("/contacts/verdict")) return { ok: true, status: 204 };
    if (String(url).includes("/contacts/unmerge")) return { ok: true, status: 204 };
    if (String(url).includes("/contacts/merges")) {
      return { ok: true, status: 200, json: async () => merges };
    }
    if (String(url).includes("/proposals/")) return { ok: true, status: 200, json: async () => ({}) };
    if (String(url).includes("/contacts")) {
      return { ok: true, status: 200, json: async () => contacts };
    }
    return { ok: false, status: 404 };
  });
}

async function settle() {
  await act(async () => {});
}

describe("who writes to you", () => {
  beforeEach(() => fetchMock.mockReset());
  afterEach(() => fetchMock.mockReset());

  it("shows a standing decision that the message which prompted it no longer can", async () => {
    rosterOf([
      who({ address: "noisy@example.com", display_name: null, verdict: "mute" }),
      who({ address: "maria@example.com", verdict: null }),
    ]);

    render(<Senders token="t" />);
    await settle();

    // Pinning happens on a message. Once that message scrolls out of the queue, the only trace of
    // the decision is its effect — which is the difference between a rule and one you can audit.
    expect(screen.getByText("always noise")).toBeTruthy();
    expect(screen.getByText(/2 known · 1 decided/)).toBeTruthy();
  });

  it("marks the people you have written back to", async () => {
    rosterOf([who({ outbound_ever: 1 })]);
    render(<Senders token="t" />);
    await settle();

    // Not decoration: `priority.rs` demotes a first-contact "urgent" and leaves a correspondent's
    // alone, so this is the fact that explains why two identical messages were classified apart.
    expect(screen.getByText("you write back")).toBeTruthy();
  });

  it("toggles a decision straight from the list", async () => {
    rosterOf([who({ verdict: null })]);
    render(<Senders token="t" />);
    await settle();

    fireEvent.click(screen.getByRole("button", { name: "Always noise" }));
    await settle();

    const write = fetchMock.mock.calls.find(([url]) =>
      String(url).includes("/contacts/verdict"),
    ) as [string, RequestInit];
    expect(JSON.parse(String(write[1].body))).toEqual({
      address: "maria@example.com",
      verdict: "mute",
    });
  });

  it("filters without asking the daemon again", async () => {
    rosterOf([
      who({ address: "maria@example.com", display_name: "Maria" }),
      who({ address: "joao@example.com", display_name: "João" }),
    ]);
    render(<Senders token="t" />);
    await settle();

    const before = fetchMock.mock.calls.length;
    fireEvent.change(screen.getByPlaceholderText("an address or a name"), {
      target: { value: "joao" },
    });
    await settle();

    expect(screen.queryByText("Maria")).toBeNull();
    expect(screen.getByText("João")).toBeTruthy();
    // A couple of hundred rows are already in hand; asking again per keystroke would buy latency
    // and nothing else.
    expect(fetchMock.mock.calls.length).toBe(before);
  });
});

describe("answering whether two addresses are one person", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    fetchMock.mockReset();
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
  });

  /** ConfirmButton swallows a second click inside 400ms as a double-click accident. */
  async function confirm(name: string, confirmLabel: string) {
    fireEvent.click(screen.getByRole("button", { name }));
    await act(async () => { vi.advanceTimersByTime(400); });
    fireEvent.click(screen.getByRole("button", { name: confirmLabel }));
    await settle();
  }

  it("puts both sides' addresses on screen, because that is the question", async () => {
    rosterOf([who()], [suggestion()]);
    render(<Senders token="t" />);
    await settle();

    // Nobody can answer "are contacts 1 and 2 the same person". An address you do not recognise is
    // the whole reason to answer no, so every address under each contact is shown.
    expect(screen.getByText("m.silva@example.com")).toBeTruthy();
    expect(screen.getByText("share a name")).toBeTruthy();
  });

  it("approves through the proposal it came from", async () => {
    rosterOf([who()], [suggestion({ proposal_id: 9 })]);
    render(<Senders token="t" />);
    await settle();

    await confirm("Same person", "Confirm same person?");

    const decided = fetchMock.mock.calls.map(([url]) => String(url));
    expect(decided.some((url) => url.endsWith("/proposals/9/approve"))).toBe(true);
  });

  it("names the way out when two standing decisions contradict", async () => {
    rosterOf([who()], [suggestion({ keep: side({ verdict: "pin" }), absorb: side({ contact_id: 2, verdict: "mute" }) })]);
    render(<Senders token="t" />);
    await settle();
    fetchMock.mockImplementationOnce(async () => ({ ok: false, status: 409 }));

    await confirm("Same person", "Confirm same person?");

    // The daemon refuses rather than picking a winner and discarding one instruction. A bare
    // "failed" would leave the person with no idea that withdrawing a pin is what unblocks it.
    expect(screen.getByText(/Withdraw one of them below/)).toBeTruthy();
  });

  it("remembers a refusal instead of asking again", async () => {
    rosterOf([who()], [suggestion({ proposal_id: 9 })]);
    render(<Senders token="t" />);
    await settle();

    fireEvent.click(screen.getByRole("button", { name: "Different people" }));
    await settle();

    // Rejecting goes through the proposal too, which is what records the pair — without it the
    // heuristic proposes the same pair on every sweep, forever.
    const decided = fetchMock.mock.calls.map(([url]) => String(url));
    expect(decided.some((url) => url.endsWith("/proposals/9/reject"))).toBe(true);
  });

  it("offers the undo only where a human actually joined something", async () => {
    rosterOf([
      who({ address: "a@example.com", contact_id: 5, linked_by: "human" }),
      who({ address: "b@example.com", contact_id: 5, linked_by: "human" }),
      who({ address: "alone@example.com", contact_id: 6, linked_by: "implicit" }),
    ]);
    render(<Senders token="t" />);
    await settle();

    // Two rows share contact 5, so both can be split back out. The lone address has nothing to
    // undo — and `linked_by` alone would keep claiming a merge after the other half was split off.
    expect(screen.getAllByRole("button", { name: "Not the same person" })).toHaveLength(2);
    expect(screen.getAllByText("merged")).toHaveLength(2);
  });

  it("splits an address back out by address, not by contact", async () => {
    rosterOf([
      who({ address: "a@example.com", contact_id: 5, linked_by: "human" }),
      who({ address: "b@example.com", contact_id: 5, linked_by: "human" }),
    ]);
    render(<Senders token="t" />);
    await settle();

    fireEvent.click(screen.getAllByRole("button", { name: "Not the same person" })[1]!);
    await act(async () => { vi.advanceTimersByTime(400); });
    fireEvent.click(screen.getByRole("button", { name: "Confirm split?" }));
    await settle();

    const split = fetchMock.mock.calls.find(([url]) =>
      String(url).includes("/contacts/unmerge"),
    ) as [string, RequestInit];
    // The address is what moves — splitting the contact would be ambiguous once three addresses
    // share one.
    expect(JSON.parse(String(split[1].body))).toEqual({ address: "b@example.com" });
  });
});
