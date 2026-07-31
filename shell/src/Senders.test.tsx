import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Senders from "./Senders";
import type { Correspondent } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function who(overrides: Partial<Correspondent> = {}): Correspondent {
  return {
    address: "maria@example.com", display_name: "Maria", messages_in: 12,
    outbound_ever: 0, first_seen: "2026-01-01T10:00:00Z", last_seen: "2026-07-29T10:00:00Z",
    verdict: null,
    ...overrides,
  };
}

function rosterOf(contacts: Correspondent[]) {
  fetchMock.mockImplementation(async (url: string) => {
    if (String(url).includes("/contacts/verdict")) return { ok: true, status: 204 };
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
