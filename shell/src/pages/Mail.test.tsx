import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Mail } from "./Mail";
import { keys } from "../data/keys";
import type { EmailConfigView, MailCursor, QueuedEmail, TriageOutcome } from "../data/mail";
import { useContactMerges, useWheelRequests } from "../data/waiting";
import { renderWithQuery, renderWithRouter } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

/* ------------------------------------------------------------- fixtures -- */

function queuedEmail(overrides: Partial<QueuedEmail> = {}): QueuedEmail {
  return {
    id: 1,
    from_addr: "ana@example.com",
    from_name: "Ana",
    subject: "hello",
    received_at: "2026-08-17T09:00:00Z",
    triage_class: null,
    triage_summary: null,
    triaged_at: null,
    has_attachments: 0,
    sender_verdict: null,
    ...overrides,
  };
}

function emailConfig(overrides: Partial<EmailConfigView> = {}): EmailConfigView {
  return {
    enabled: true,
    armed: true,
    host: "imap.example.com",
    username: "ana",
    mailbox: "INBOX",
    sent_mailbox: null,
    poll_interval_secs: 300,
    notify_classes: ["urgent"],
    digest_hour_utc: 8,
    retain_bodies_days: 30,
    local_triage_disabled: null,
    ...overrides,
  };
}

interface MailWorld {
  queue: QueuedEmail[];
  config: EmailConfigView;
  cursor: MailCursor;
  triageOutcome: TriageOutcome;
}

function mailWorld(overrides: Partial<MailWorld> = {}): MailWorld {
  return {
    queue: [],
    config: emailConfig(),
    cursor: null,
    triageOutcome: { queued: 0, run_id: null, reason: "nothing is waiting to be triaged" },
    ...overrides,
  };
}

/**
 * The three GET routes this page reads, plus the one write, over mutable
 * state. `path.split("?")[0]` because `/email/queue` and `/email/cursor` both
 * carry a query string this responder does not need to parse to answer.
 */
function mailFetch(world: MailWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    if (init?.method === "POST" && path === "/email/triage") return world.triageOutcome;
    const route = path.split("?")[0];
    switch (route) {
      case "/email/queue":
        return world.queue;
      case "/config/email":
        return world.config;
      case "/email/cursor":
        return world.cursor;
      default:
        return undefined;
    }
  };
}

/**
 * The page inside a real router, and nothing else — `Mail` links to `/feed`,
 * so it needs router context, and `renderApp` would mount the gate and the
 * rail's own live queries around every assertion for no benefit here.
 */
function renderMail(initialPath = "/mail") {
  return renderWithRouter(<Mail />, { initialPath });
}

describe("Mail — panel order", () => {
  it("puts the queue first and configuration last", async () => {
    daemon.apiFetch.mockImplementation(mailFetch(mailWorld()));

    await renderMail();

    await screen.findByRole("heading", { level: 2, name: "Queue" });
    const headings = [...document.querySelectorAll("h2")].map((heading) => heading.textContent);
    expect(headings[0]).toBe("Queue");
    expect(headings[headings.length - 1]).toBe("Configuration");
  });
});

/* -------------------------------------------------- an untriaged message -- */

describe("Mail — an untriaged message", () => {
  it("shows an untriaged message as not triaged rather than as clean", async () => {
    const world = mailWorld({
      queue: [
        queuedEmail({ id: 1, subject: "server down", triage_class: null }),
        queuedEmail({ id: 2, subject: "newsletter", triage_class: "noise" }),
      ],
    });
    daemon.apiFetch.mockImplementation(mailFetch(world));

    await renderMail();

    const subject = await screen.findByText("server down");
    const card = subject.closest("li");
    if (card === null) throw new Error("the row was not found");

    // NULL is triage never having reached this message — a different fact
    // from `noise`, which is triage having read it and found nothing. The
    // two must not read as the same badge.
    const badge = within(card).getByText(/not triaged/i);
    expect(badge.className).toContain("ui-badge-info");
    expect(badge.textContent?.toLowerCase()).not.toContain("noise");
    expect(badge.textContent?.toLowerCase()).not.toContain("clean");
  });
});

/* ------------------------------------------------------- a triage refusal -- */

describe("Mail — a triage refusal", () => {
  it("renders a triage refusal sentence returned on a 200 as a value, not an error", async () => {
    const world = mailWorld({
      triageOutcome: { queued: 0, run_id: null, reason: "the email pillar is not armed" },
    });
    daemon.apiFetch.mockImplementation(mailFetch(world));

    await renderMail();
    fireEvent.click(await screen.findByRole("button", { name: "Run triage now" }));

    // Every refusal `POST /email/triage` makes is a 200 with a sentence in
    // `reason` — never an `ApiRefusal`, so it must never land inside the
    // error or refusal chrome.
    const note = await screen.findByText("the email pillar is not armed");
    expect(note.getAttribute("role")).toBe("status");
    expect(note.closest(".ui-note-error")).toBeNull();
    expect(note.closest(".ui-note-refusal")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

/* ------------------------------------------------------------- the badge -- */

describe("Mail — the untriaged count", () => {
  it("carries no mail badge until the queue has answered once", async () => {
    const world = mailWorld();
    let resolveQueue: (rows: QueuedEmail[]) => void = () => {};
    const queuePromise = new Promise<QueuedEmail[]>((resolve) => {
      resolveQueue = resolve;
    });
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path.split("?")[0] === "/email/queue") return await queuePromise;
      return await mailFetch(world)(path, init);
    });

    await renderMail();

    // The queue has not answered yet — there is nothing to have measured a
    // count from, and a badge here would be a claim nobody checked.
    expect(screen.queryByText(/untriaged/)).toBeNull();

    resolveQueue([queuedEmail({ id: 1, triage_class: null })]);

    expect(await screen.findByText(/untriaged/)).toBeDefined();
  });
});

describe("Mail — an unconfigured account", () => {
  it("says so instead of rendering empty configuration facts", async () => {
    const absentConfig: Partial<EmailConfigView> = emailConfig();
    delete absentConfig.username;
    delete absentConfig.host;
    delete absentConfig.mailbox;
    const world = mailWorld({ config: absentConfig as EmailConfigView });
    daemon.apiFetch.mockImplementation(mailFetch(world));

    await renderMail();

    expect(await screen.findByText("no account configured")).toBeDefined();
    expect(screen.getByText("no mailbox named")).toBeDefined();
    expect(screen.getByText("nothing is wrong")).toBeDefined();
    const facts = document.querySelectorAll(".mail-config dd");
    expect([...facts].some((fact) => fact.textContent?.includes("undefined"))).toBe(false);
  });

  it("renders the local triage disable reason and calls an absent value unknown", async () => {
    const reasonConfig = emailConfig({ local_triage_disabled: "the local model is unavailable" });
    daemon.apiFetch.mockImplementation(mailFetch(mailWorld({ config: reasonConfig })));

    await renderMail();

    expect(await screen.findByText("disabled: the local model is unavailable")).toBeDefined();

    const absentConfig: Partial<EmailConfigView> = emailConfig();
    delete absentConfig.local_triage_disabled;
    daemon.apiFetch.mockImplementation(mailFetch(mailWorld({ config: absentConfig as EmailConfigView })));

    await renderMail();

    expect(await screen.findByText("unknown")).toBeDefined();
  });
});

/* ---------------------------------------------- the pillar key migration -- */

/**
 * Not about the Mail page's own rendering — about `data/waiting.ts`'s
 * retirement of `WAITING_KEYS` in this same slice, which this file is the
 * only allowed place to prove: `Waiting.test.tsx` is off limits to this
 * packet precisely so the migration cannot lean on it.
 */
describe("Mail — the pillar key migration", () => {
  it("reads the browser and contacts queues through the registered pillar keys", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/browser/sessions") return [];
      if (path === "/contacts/merges") return [];
      return undefined;
    });

    function Probe() {
      useWheelRequests();
      useContactMerges();
      return null;
    }

    const { queryClient } = renderWithQuery(<Probe />);

    await waitFor(() => {
      expect(
        queryClient.getQueryCache().find({ queryKey: keys.browser.sessions, exact: true }),
      ).toBeDefined();
      expect(
        queryClient.getQueryCache().find({ queryKey: keys.contacts.merges, exact: true }),
      ).toBeDefined();
    });
  });
});

describe("Mail search field", () => {
  it("the search label is the field primitive", async () => {
    daemon.apiFetch.mockImplementation(mailFetch(mailWorld()));

    await renderMail();

    expect(await screen.findByRole("search", { name: "Search the mail queue" })).toBeDefined();
    const input = screen.getByLabelText("Search sender, subject or summary");
    // The field is a column around a `<label for>` and the control: the label holds its own text
    // and nothing else, so a helper can never become part of the control's name.
    const field = input.closest(".ui-field");
    expect(field).not.toBeNull();
    const label = field?.querySelector("label");
    expect(label?.className).toContain("ui-field-label");
    expect(label?.textContent).toBe("Search");
    expect(label?.htmlFor).toBe(input.id);
    fireEvent.change(input, { target: { value: "invoice" } });
    fireEvent.submit(screen.getByRole("search", { name: "Search the mail queue" }));
    const actions = screen.getByRole("button", { name: "Search" }).closest(".mail-search-actions");
    expect(actions?.className).toContain("mail-search-actions");
    expect(actions?.querySelectorAll("button")).toHaveLength(2);
  });

  /**
   * The buttons centre on the input by arithmetic, and the arithmetic is in tokens.
   *
   * jsdom lays nothing out, so the sheet is where this is checkable. The number that used to be
   * typed out here was right — the input's font size times the inherited leading, plus its
   * padding twice, plus two borders — but any one of those tokens changing moved the input and
   * left the buttons where they were.
   */
  it("the search buttons take their height from the input's tokens", () => {
    const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "mail.css"), "utf8");
    const rule = /\.mail-search-actions\s*\{([^}]*)\}/.exec(css);
    expect(rule).not.toBeNull();
    expect(rule?.[1]).toMatch(/min-height:\s*calc\(/);
    expect(rule?.[1]).toContain("var(--text-sm)");
    expect(rule?.[1]).toContain("var(--leading-normal)");
    expect(rule?.[1]).toContain("var(--space-2)");
    expect(css).not.toContain("2.384375");
  });
});
