import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Mail from "./Mail";
import type { QueuedEmail } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function message(overrides: Partial<QueuedEmail> = {}): QueuedEmail {
  return {
    id: 1, from_addr: "maria@example.com", from_name: "Maria",
    subject: "the roof", received_at: "2026-07-30T10:00:00Z",
    triage_class: "noise", triage_summary: null, triaged_at: "2026-07-30T10:01:00Z",
    has_attachments: 0, sender_verdict: null,
    ...overrides,
  };
}

/** A daemon holding this queue, with every write succeeding unless a test says otherwise. */
function mailboxOf(queue: QueuedEmail[]) {
  fetchMock.mockImplementation(async (url: string) => {
    const target = String(url);
    if (target.includes("/email/queue")) {
      return { ok: true, status: 200, json: async () => queue };
    }
    if (target.includes("/email/cursor")) {
      return { ok: true, status: 200, json: async () => null };
    }
    if (target.includes("/contacts/verdict")) return { ok: true, status: 204 };
    if (target.includes("/mail-files")) return { ok: true, status: 200, json: async () => [] };
    return { ok: true, status: 200, json: async () => [] };
  });
}

async function settle() {
  await act(async () => {});
}

function verdictCalls() {
  return fetchMock.mock.calls.filter(([url]) => String(url).includes("/contacts/verdict"));
}

function bodyOf(call: number): unknown {
  const [, init] = verdictCalls()[call - 1] as [string, RequestInit];
  return JSON.parse(String(init.body));
}

describe("deciding about a sender once, for all their mail", () => {
  beforeEach(() => fetchMock.mockReset());
  afterEach(() => fetchMock.mockReset());

  it("records a pin against the address the message came from", async () => {
    mailboxOf([message()]);
    render(<Mail token="t" connection="connected" />);
    await settle();

    fireEvent.click(screen.getByRole("button", { name: "Always urgent" }));
    await settle();

    expect(bodyOf(1)).toEqual({ address: "maria@example.com", verdict: "pin" });
  });

  it("withdraws the decision when the active choice is pressed again", async () => {
    mailboxOf([message({ sender_verdict: "pin" })]);
    render(<Mail token="t" connection="connected" />);
    await settle();

    const pin = screen.getByRole("button", { name: "Always urgent" });
    // The current state is on screen already, so the way to undo it is the control that shows it —
    // a separate "clear" button would be a third way to say something already visible.
    expect(pin.getAttribute("aria-pressed")).toBe("true");

    fireEvent.click(pin);
    await settle();
    expect(bodyOf(1)).toEqual({ address: "maria@example.com", verdict: null });
  });

  it("switches straight from one standing decision to the other", async () => {
    mailboxOf([message({ sender_verdict: "pin" })]);
    render(<Mail token="t" connection="connected" />);
    await settle();

    fireEvent.click(screen.getByRole("button", { name: "Always noise" }));
    await settle();

    // Not a clear followed by a set: the daemon upserts on the contact, so one request holds the
    // whole change and there is no moment in between where the sender has no decision.
    expect(verdictCalls()).toHaveLength(1);
    expect(bodyOf(1)).toEqual({ address: "maria@example.com", verdict: "mute" });
  });

  it("says the decision governs what comes next, not what is already classified", async () => {
    mailboxOf([message()]);
    render(<Mail token="t" connection="connected" />);
    await settle();

    fireEvent.click(screen.getByRole("button", { name: "Always urgent" }));
    await settle();

    // Without this, someone pins a sender, sees the message in front of them still marked noise,
    // and concludes the button did nothing. The note says what changed and names the control that
    // applies it to mail already read — which is why it must mention both.
    const note = screen.getByText(/New mail from this sender will be urgent/);
    expect(note.textContent).toContain("Read again");
  });

  it("keeps the note with the sender it was about", async () => {
    mailboxOf([
      message({ id: 1, from_addr: "maria@example.com", from_name: "Maria" }),
      message({ id: 2, from_addr: "joao@example.com", from_name: "João" }),
    ]);
    render(<Mail token="t" connection="connected" />);
    await settle();

    fireEvent.click(screen.getAllByRole("button", { name: "Always urgent" })[0]!);
    await settle();

    // One note, under one sender. A bare string in state would have printed it under every row,
    // including the messages the decision had nothing to do with.
    expect(screen.getAllByText(/New mail from this sender will be urgent/)).toHaveLength(1);
  });
});
