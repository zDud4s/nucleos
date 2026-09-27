import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// `apiBlob` is in this hoisted object because `AttachmentsPanel` reaches it
// through `useDownloadAttachment` — a component mounting anything that calls
// it needs the mock present or the real client tries to read a daemon token
// through Tauri's `invoke`, which is not what these tests are about.
const daemon = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  apiBlob: vi.fn(),
  probeHealth: vi.fn(),
}));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { MailDetail } from "./MailDetail";
import { ApiRefusal } from "../data/client";
import { createAppQueryClient } from "../app/queryClient";
import type { EmailDetail } from "../data/mail";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.apiBlob.mockReset();
  daemon.probeHealth.mockReset();
});

/* ------------------------------------------------------------- fixtures -- */

function emailDetail(overrides: Partial<EmailDetail> = {}): EmailDetail {
  return {
    id: 42,
    from_addr: "ana@example.com",
    from_name: "Ana",
    subject: "quarterly numbers",
    received_at: "2026-08-17T09:00:00Z",
    triage_class: "action",
    triage_summary: "needs a reply about Q3",
    triaged_at: "2026-08-17T09:05:00Z",
    model_class: "action",
    priority_rule: null,
    body_text: "Here are the numbers.",
    has_attachments: 0,
    sender_verdict: null,
    attachments: [],
    ...overrides,
  };
}

/**
 * The page under a router that knows its two routes, and nothing else —
 * `Projects.test.tsx`'s pattern. `renderApp` would mount the gate, the rail
 * and every live query around it for no benefit here, and the harness's own
 * `renderWithRouter` has no `$emailId` route to give a param to.
 */
async function renderMailDetail(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({
      getParentRoute: () => rootRoute,
      path: "/mail",
      component: () => <p data-testid="route-marker">/mail</p>,
    }),
    createRoute({ getParentRoute: () => rootRoute, path: "/mail/$emailId", component: MailDetail }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
    defaultPreload: false,
  });

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient };
}

/** The dwell `ConfirmButton` needs between arming and confirming — a real gap. */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

/* ------------------------------------------------------------ a pruned body -- */

describe("MailDetail — a pruned body", () => {
  it("says a pruned body is gone and offers no requeue", async () => {
    const detail = emailDetail({ body_text: null });
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/email/42") return detail;
      return undefined;
    });

    await renderMailDetail("/mail/42");

    // The body panel says what happened to it. Both the body panel and the
    // facts panel explain "pruned by retention" in their own words, at the
    // two places a person would otherwise look for what is missing — so the
    // assertions below match each panel's own sentence rather than the
    // shared phrase, which resolves to two elements.
    expect(await screen.findByText(/only the facts above remain/)).toBeDefined();
    // ...and eligibility is read off `body_text`, never the triage class —
    // this message is `action`-classified and would otherwise look
    // requeueable by every other signal on the page.
    expect(await screen.findByText(/requeuing is not offered/)).toBeDefined();
    expect(screen.queryByRole("button", { name: "Requeue for triage" })).toBeNull();
  });
});

/* --------------------------------------------------------- the sender -- */

describe("MailDetail — the standing decision about a sender", () => {
  it("says which way the sender is set, and marks the button that is the setting", async () => {
    const detail = emailDetail({ sender_verdict: "pin" });
    daemon.apiFetch.mockImplementation(async (path: string) => (path === "/email/42" ? detail : undefined));

    await renderMailDetail("/mail/42");

    // The page could not say this at all before: `EmailDetail` carried no verdict, so a sender
    // the queue had just shown as pinned opened onto three buttons with nothing marked.
    expect(await screen.findByText(/is pinned — their mail keeps being surfaced/)).toBeDefined();
    expect(screen.getByRole("button", { name: "Pin" }).getAttribute("aria-pressed")).toBe("true");
    // Clear is the way back out, and exists only when there is something to clear.
    expect(screen.getByRole("button", { name: "Clear" })).toBeDefined();
  });

  it("offers no way back out when nothing has been decided", async () => {
    const detail = emailDetail({ sender_verdict: null });
    daemon.apiFetch.mockImplementation(async (path: string) => (path === "/email/42" ? detail : undefined));

    await renderMailDetail("/mail/42");

    expect(await screen.findByText(/No standing decision about/)).toBeDefined();
    expect(screen.getByRole("button", { name: "Pin" }).getAttribute("aria-pressed")).toBe("false");
    expect(screen.queryByRole("button", { name: "Clear" })).toBeNull();
  });

  it("asks twice before muting, because muted mail stops being surfaced silently", async () => {
    const detail = emailDetail({ sender_verdict: null });
    const posted: unknown[] = [];
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/contacts/verdict" && init?.method === "POST") {
        posted.push(JSON.parse(String(init.body)));
        return undefined;
      }
      return path === "/email/42" ? detail : undefined;
    });

    await renderMailDetail("/mail/42");

    fireEvent.click(await screen.findByRole("button", { name: "Mute" }));
    // One press arms it and sends nothing — the consequence is named on the armed label.
    expect(posted).toHaveLength(0);
    const armed = await screen.findByRole("button", { name: /Mute them now/ });
    await afterDwell();
    fireEvent.click(armed);

    await waitFor(() => expect(posted).toHaveLength(1));
    expect(posted[0]).toEqual({ address: "ana@example.com", verdict: "mute" });
    // And the outcome names what was recorded, rather than the bare word "recorded".
    expect(await screen.findByText("recorded — this sender is muted")).toBeDefined();
  });
});

/* ------------------------------------------------------- what triage said -- */

describe("MailDetail — the model's own class", () => {
  it("shows it only when it disagrees with the class that was stored", async () => {
    const agreeing = emailDetail({ triage_class: "action", model_class: "action" });
    daemon.apiFetch.mockImplementation(async (path: string) => (path === "/email/42" ? agreeing : undefined));

    const { unmount } = await renderMailDetail("/mail/42");
    // The badge above already says "needs a reply, not today"; printing the model's identical
    // answer beside it is one fact under two names.
    expect(await screen.findByText("Triage")).toBeDefined();
    expect(screen.queryByText("Model said")).toBeNull();
    unmount();

    const disagreeing = emailDetail({ triage_class: "action", model_class: "urgent", priority_rule: "first-contact" });
    daemon.apiFetch.mockImplementation(async (path: string) => (path === "/email/42" ? disagreeing : undefined));

    await renderMailDetail("/mail/42");

    expect(await screen.findByText("Model said")).toBeDefined();
    expect(screen.getByText("Overridden by")).toBeDefined();
  });
});

/* -------------------------------------------------------- saved attachment -- */

describe("MailDetail — attachments", () => {
  it("reports the filename the daemon actually wrote, not the one it was sent", async () => {
    const detail = emailDetail({
      has_attachments: 1,
      attachments: [{ position: 0, filename: "invoice.pdf", mime_type: "application/pdf", size_bytes: 2048 }],
    });
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/email/42") return detail;
      if (path === "/email/42/attachments/0/save" && init?.method === "POST") {
        // Sanitised and de-collided by the daemon — a different string from
        // what the sender actually called it.
        return { filename: "invoice(1).pdf", folder: "" };
      }
      return undefined;
    });

    await renderMailDetail("/mail/42");

    // The sender's own name is what labels the row before anything is saved.
    expect(await screen.findByText("invoice.pdf")).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Save to files" }));

    // The daemon's answer lands through a mutation, so this is a `findBy*`
    // rather than a `getBy*` — react-query settles it a tick later.
    expect(await screen.findByText("invoice(1).pdf")).toBeDefined();
    // And the sender's name is still on screen, unclobbered by the daemon's —
    // the two are different facts, shown side by side, not one overwriting
    // the other.
    expect(screen.getByText("invoice.pdf")).toBeDefined();
  });
});

/* -------------------------------------------------------------- sending -- */

describe("MailDetail — sending a reply", () => {
  it("turns the SMTP 503 into a named refusal beside the reply form", async () => {
    const detail = emailDetail();
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/email/42") return detail;
      if (path === "/email/send" && init?.method === "POST") {
        // `POST /email/send` refuses in bare prose the daemon wrote on
        // purpose — worth quoting verbatim rather than translated.
        throw new ApiRefusal(
          503,
          "unavailable",
          "no submission host is configured — set smtp_host in ~/.nucleos/email.yaml",
        );
      }
      return undefined;
    });

    await renderMailDetail("/mail/42");

    fireEvent.change(await screen.findByLabelText("Reply body"), { target: { value: "on it, will send tomorrow" } });

    // Arm, then confirm — two separate gestures, genuinely apart in time so
    // the second click does not land inside the interlock's 300ms dwell.
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Send it now" }));

    // The daemon's own sentence, verbatim — not the shell's generic
    // "the part of the núcleo this needs is not available" reading of a 503.
    expect(
      await screen.findByText("no submission host is configured — set smtp_host in ~/.nucleos/email.yaml"),
    ).toBeDefined();
    expect(screen.queryByText(/this shell has no reading/)).toBeNull();
  });
});

describe("MailDetail — an absent subject", () => {
  it("replies to a message whose subject the daemon never sent", async () => {
    const detail = emailDetail();
    delete (detail as { subject?: unknown }).subject;
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/email/42") return detail;
      return undefined;
    });

    await renderMailDetail("/mail/42");

    expect(await screen.findByText("Reply")).toBeDefined();
    expect((screen.getByLabelText("Reply subject") as HTMLInputElement).value).toBe("Re:");
  });
});
