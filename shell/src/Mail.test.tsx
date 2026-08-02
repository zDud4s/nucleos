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

function emailConfig(overrides: Record<string, unknown> = {}) {
  return {
    enabled: true, armed: true, host: "imap.example.com", username: "me@example.com",
    mailbox: "INBOX", sent_mailbox: null, poll_interval_secs: 300,
    notify_classes: ["urgent"], digest_hour_utc: 7, retain_bodies_days: 14,
    local_triage_disabled: null,
    ...overrides,
  };
}

/** A daemon holding this queue, with every write succeeding unless a test says otherwise. */
function mailboxOf(queue: QueuedEmail[], config: Record<string, unknown> = emailConfig()) {
  fetchMock.mockImplementation(async (url: string) => {
    const target = String(url);
    if (target.includes("/config/email")) {
      return { ok: true, status: 200, json: async () => config };
    }
    if (target.includes("/email/queue")) {
      return { ok: true, status: 200, json: async () => queue };
    }
    if (target.includes("/email/cursor")) {
      return { ok: true, status: 200, json: async () => null };
    }
    if (target.includes("/contacts/verdict")) return { ok: true, status: 204 };
    // The folder suggestions in the filing box, which the Files tab now browses in full.
    if (target.includes("/files")) return { ok: true, status: 200, json: async () => [] };
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

describe("the mailbox the daemon actually collects from", () => {
  beforeEach(() => fetchMock.mockReset());
  afterEach(() => fetchMock.mockReset());

  it("asks the cursor for the configured mailbox, not for INBOX", async () => {
    mailboxOf([], emailConfig({ mailbox: "Trabalho" }));
    render(<Mail token="t" connection="connected" />);
    await settle();

    // Guessing INBOX made the cursor read empty for anyone collecting elsewhere — and "nothing
    // collected yet" is what a healthy idle mailbox says too, so the mistake was unreadable.
    const cursorCalls = fetchMock.mock.calls
      .map(([url]) => String(url))
      .filter((url) => url.includes("/email/cursor"));
    expect(cursorCalls.some((url) => url.includes("Trabalho"))).toBe(true);
    expect(screen.getByText(/Trabalho cursor/)).toBeTruthy();
  });

  it("says when collection is on but nothing is being read", async () => {
    mailboxOf([], emailConfig({ enabled: true, armed: false }));
    render(<Mail token="t" connection="connected" />);
    await settle();

    // Enabled is not armed. Mail keeps arriving and keeps expiring while triage refuses to run,
    // which from the outside looks like a button that does nothing.
    expect(screen.getByText(/triage is not armed/)).toBeTruthy();
  });

  it("says when local triage was asked for and could not be given", async () => {
    mailboxOf([], emailConfig({ local_triage_disabled: "the model could not hold the prompt" }));
    render(<Mail token="t" connection="connected" />);
    await settle();

    // The whole point of asking for a local model is that bodies stay on the machine, so falling
    // back to the remote CLI would break the promise exactly when nobody is watching. It stops
    // instead — and that has to be legible, or the mailbox just looks stuck.
    expect(screen.getByText(/could not hold the prompt/)).toBeTruthy();
  });
});
