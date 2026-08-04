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

/**
 * Answering a message — the one control in this app behind which something leaves the machine.
 *
 * These tests are about what the form REFUSES and what it REFUSES TO CLAIM, in that order. The
 * interlock is covered on its own in `ui/ConfirmButton.test.tsx`; what is covered here is that the
 * send is actually behind it, and that a failure the daemon reported as attempted is never
 * described to the person as "not sent".
 */
describe("answering a message", () => {
  beforeEach(() => {
    fetchMock.mockReset();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
  });

  function detail(overrides: Record<string, unknown> = {}) {
    return {
      id: 1, from_addr: "maria@example.com", from_name: "Maria",
      subject: "the roof", received_at: "2026-07-30T10:00:00Z",
      triage_class: "noise", triage_summary: null, triaged_at: "2026-07-30T10:01:00Z",
      body_text: "the tiles are loose", has_attachments: 0, attachments: [],
      ...overrides,
    };
  }

  /** A mailbox holding one message, and a daemon that answers `/email/send` as the test dictates. */
  function mailboxWithSend(
    send: { ok: boolean; status: number; reason?: string } = { ok: true, status: 204 },
    message_ = detail(),
  ) {
    fetchMock.mockImplementation(async (url: string) => {
      const target = String(url);
      if (target.includes("/email/send")) {
        return { ok: send.ok, status: send.status, text: async () => send.reason ?? "" };
      }
      if (target.includes("/config/email")) {
        return { ok: true, status: 200, json: async () => emailConfig() };
      }
      if (target.includes("/email/queue")) {
        return { ok: true, status: 200, json: async () => [message({ id: 1 })] };
      }
      if (target.includes("/email/cursor")) return { ok: true, status: 200, json: async () => null };
      if (/\/email\/1(\?|$)/.test(target)) {
        return { ok: true, status: 200, json: async () => message_ };
      }
      return { ok: true, status: 200, json: async () => [] };
    });
  }

  function sendCalls() {
    return fetchMock.mock.calls.filter(([url]) => String(url).includes("/email/send"));
  }

  /** Opens the message, then the reply form. Returns nothing: the fields are read off the screen. */
  async function openReply() {
    render(<Mail token="t" connection="connected" />);
    await settle();
    fireEvent.click(screen.getByRole("button", { name: /the roof/ }));
    await settle();
    fireEvent.click(screen.getByRole("button", { name: "Reply" }));
    await settle();
  }

  function field(label: string): HTMLInputElement | HTMLTextAreaElement {
    return screen.getByLabelText(label) as HTMLInputElement | HTMLTextAreaElement;
  }

  it("does not offer a reply box until it is asked for", async () => {
    mailboxWithSend();
    render(<Mail token="t" connection="connected" />);
    await settle();
    fireEvent.click(screen.getByRole("button", { name: /the roof/ }));
    await settle();

    // A box that is always open is a box that gets typed into by accident, and this one sends.
    expect(screen.queryByLabelText("To")).toBeNull();
    expect(screen.getByRole("button", { name: "Reply" })).toBeTruthy();
  });

  it("fills the reply from the message it is answering", async () => {
    mailboxWithSend();
    await openReply();

    expect(field("To").value).toBe("maria@example.com");
    expect(field("Subject").value).toBe("Re: the roof");
    // The original is quoted, and the answer goes above it.
    expect(field("Message").value).toContain("Maria wrote:");
    expect(field("Message").value).toContain("> the tiles are loose");
  });

  it("quotes nothing when retention already took the body", async () => {
    mailboxWithSend({ ok: true, status: 204 }, detail({ body_text: null }));
    await openReply();

    // Quoting an empty block would have the reply assert that Maria wrote nothing.
    expect(field("Message").value).toBe("");
    expect(field("Subject").value).toBe("Re: the roof");
  });

  it("will not send on one press", async () => {
    mailboxWithSend();
    await openReply();

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await settle();

    // Armed, and nothing has left the machine.
    expect(sendCalls()).toHaveLength(0);
    expect(screen.getByRole("button", { name: "Send it?" })).toBeTruthy();
  });

  it("sends what is on screen once the second press confirms it", async () => {
    mailboxWithSend();
    await openReply();

    fireEvent.change(field("Message"), { target: { value: "Thursday works." } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    act(() => { vi.advanceTimersByTime(400); });
    fireEvent.click(screen.getByRole("button", { name: "Send it?" }));
    await settle();

    expect(sendCalls()).toHaveLength(1);
    const [, init] = sendCalls()[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({
      to: "maria@example.com", subject: "Re: the roof", body: "Thursday works.",
    });
    // The form closes and says where it went, rather than sitting there ready to send twice.
    expect(screen.queryByLabelText("To")).toBeNull();
    expect(screen.getByText(/Sent to maria@example.com/)).toBeTruthy();
  });

  it("keeps the draft and says what to check when the sidecar was asked and failed", async () => {
    mailboxWithSend({ ok: false, status: 502, reason: "the email sidecar could not send the message" });
    await openReply();

    fireEvent.change(field("Message"), { target: { value: "Thursday works." } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    act(() => { vi.advanceTimersByTime(400); });
    fireEvent.click(screen.getByRole("button", { name: "Send it?" }));
    await settle();

    // The text is the only copy that exists, so the form stays exactly as it was.
    expect(field("Message").value).toBe("Thursday works.");
    // And the message must not claim the send did not happen — the sidecar was asked.
    expect(screen.getByText(/sent mailbox/)).toBeTruthy();
    expect(screen.queryByText(/not attempted/)).toBeNull();
  });

  it("says plainly that nothing was attempted when the daemon is not configured to send", async () => {
    mailboxWithSend({
      ok: false, status: 503,
      reason: "no submission host is configured — set smtp_host in .ai/email.yaml",
    });
    await openReply();

    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    act(() => { vi.advanceTimersByTime(400); });
    fireEvent.click(screen.getByRole("button", { name: "Send it?" }));
    await settle();

    expect(screen.getByText(/not attempted/)).toBeTruthy();
    // The daemon's own sentence names the file to edit, which this side could not have known.
    expect(screen.getByText(/smtp_host/)).toBeTruthy();
  });

  it("asks before discarding a draft that has been typed into, and not before discarding one that has not", async () => {
    mailboxWithSend();
    await openReply();

    // Untouched: confirming the disposal of nothing is friction that teaches people to click
    // through confirmations, which is the habit the interlock beside it depends on not existing.
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    await settle();
    expect(screen.queryByLabelText("To")).toBeNull();

    // Nothing was sent, so the button is still the plain offer rather than "Reply again".
    fireEvent.click(screen.getByRole("button", { name: "Reply" }));
    await settle();
    fireEvent.change(field("Message"), { target: { value: "Thursday works." } });
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));

    expect(screen.getByRole("button", { name: "Discard this draft?" })).toBeTruthy();
    expect(field("Message").value).toBe("Thursday works.");
  });
});
