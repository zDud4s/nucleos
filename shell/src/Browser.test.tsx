import { afterEach, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Browser from "./Browser";
import type { BrowserSession, BrowserSite, Proposal } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function session(overrides: Partial<BrowserSession> = {}): BrowserSession {
  return {
    id: 1,
    sidecar_id: "s1",
    run_id: 7,
    project_id: "acme",
    profile_kind: "ephemeral",
    profile_id: "r7",
    requested_url: "https://jira.example.org/login",
    final_url: "https://jira.example.org/login",
    rule: "off-list-ephemeral",
    mode: "agent",
    refusal: null,
    proposal_id: null,
    chain: null,
    chain_decided_at: null,
    opened_at: "2026-08-16T10:00:00Z",
    closed_at: null,
    ...overrides,
  };
}

function ask(overrides: Partial<Proposal> = {}): Proposal {
  return {
    id: 42,
    kind: "browser-wheel",
    status: "pending",
    run_id: 7,
    session_id: null,
    project_id: "acme",
    tool_name: "browser_handoff",
    reasoning: "there is a login here",
    tool_input: JSON.stringify({
      session_id: 1,
      requested_url: "https://jira.example.org/login",
      final_url: "https://xn--exemp1o-9za.org/login",
      origin: "https://xn--exemp1o-9za.org:443",
    }),
    created_at: "2026-08-16T10:00:00Z",
    decided_at: null,
    ...overrides,
  };
}

function daemon({
  sessions = [] as BrowserSession[],
  proposals = [] as Proposal[],
  sites = [] as BrowserSite[],
  chain = [] as string[],
}) {
  fetchMock.mockImplementation((url: string) => {
    if (url.includes("/browser/sessions")) {
      return Promise.resolve({ ok: true, json: async () => sessions });
    }
    if (url.includes("/browser/sites/")) {
      return Promise.resolve({ ok: true, json: async () => sites });
    }
    if (url.includes("/browser/return")) {
      return Promise.resolve({ ok: true, json: async () => ({ chain }) });
    }
    if (url.includes("/browser/keep")) {
      return Promise.resolve({ ok: true, json: async () => ({ granted: chain }) });
    }
    if (url.includes("/proposals")) {
      return Promise.resolve({ ok: true, json: async () => proposals });
    }
    return Promise.resolve({ ok: true, json: async () => ({}) });
  });
}

async function show(state: Parameters<typeof daemon>[0]) {
  daemon(state);
  await act(async () => {
    render(<Browser token="t" connection="connected" />);
  });
}

afterEach(() => {
  fetchMock.mockReset();
});

/**
 * Spec §5.2 measure 1, and the reason this screen exists at all.
 *
 * The host in a wheel request was chosen by an agent whose context contains the words of the page
 * that sent it there, so a homograph is the attack this dialogue has to survive. The origin is shown
 * as the daemon stored it — punycode, with its port — because `xn--exemp1o-…` IS the information and
 * rendering it as `exemplo.org` would hand the person the attacker's own spelling.
 */
it("shows a wheel request as the literal punycode origin, not a prettified one", async () => {
  await show({ proposals: [ask()] });

  expect(screen.getByText("https://xn--exemp1o-9za.org:443")).toBeTruthy();
  expect(screen.queryByText(/exemplo\.org/)).toBeNull();
  expect(screen.getByText(/there is a login here/)).toBeTruthy();
});

/**
 * Measure 2: how the agent got there. A permission asked for from a page it followed a link to is
 * not the same request as one from an address a person typed, and the redirect is what makes the two
 * distinguishable.
 */
it("says which url was asked for and where it landed", async () => {
  await show({ proposals: [ask()] });

  expect(screen.getByText("https://jira.example.org/login")).toBeTruthy();
  expect(screen.getByText(/Asked by run 7/)).toBeTruthy();
});

/** Accepting goes through the same door every other decision does, so one race settles them all. */
it("accepts through the proposal, not through a browser route of its own", async () => {
  await show({ proposals: [ask()] });

  await act(async () => {
    fireEvent.click(screen.getByText("Take the wheel"));
  });

  const approved = fetchMock.mock.calls.find((call) =>
    String(call[0]).includes("/proposals/42/approve"),
  );
  expect(approved).toBeTruthy();
});

/**
 * Which profile a session ran in is the security fact on this screen: a project profile holds the
 * owner's logins, a throwaway holds nothing and is deleted afterwards.
 */
it("distinguishes a throwaway from the profile that holds the logins", async () => {
  await show({
    sessions: [
      session(),
      session({ id: 2, profile_kind: "project", profile_id: "acme", mode: "human" }),
    ],
  });

  expect(screen.getByText("throwaway")).toBeTruthy();
  expect(screen.getByText("acme’s profile")).toBeTruthy();
  expect(screen.getByText("you are driving")).toBeTruthy();
  expect(screen.getByText("the agent is driving")).toBeTruthy();
});

/**
 * Spec §5.3a, and the property the whole pillar rests on: handing the wheel back grants NOTHING.
 * What comes back is a set to be shown, and the grant is a separate answer — so a person who closes
 * the window without answering has granted nothing, which is the safe direction.
 */
it("asks about the chain after the wheel comes back, and grants nothing before the answer", async () => {
  await show({
    sessions: [session({ id: 2, mode: "human", profile_kind: "project", profile_id: "acme" })],
    chain: ["https://jira.example.org/login", "https://accounts.google.com/o/oauth2/auth"],
  });

  await act(async () => {
    fireEvent.click(screen.getByText("Give the wheel back"));
  });

  // The question is asked, and nothing has been kept yet.
  expect(screen.getByText("Keep these?")).toBeTruthy();
  expect(screen.getByText("https://accounts.google.com/o/oauth2/auth")).toBeTruthy();
  expect(fetchMock.mock.calls.some((call) => String(call[0]).includes("/browser/keep"))).toBe(false);

  await act(async () => {
    fireEvent.click(screen.getByText("Keep them"));
  });

  const kept = fetchMock.mock.calls.find((call) => String(call[0]).includes("/browser/keep"));
  expect(kept).toBeTruthy();
  expect(JSON.parse(String(kept?.[1].body))).toEqual({ session_id: 2, keep: true });
});

/** The other answer exists and is one click, because "keep none" must be as easy as "keep". */
it("can keep none of the chain", async () => {
  await show({
    sessions: [session({ id: 2, mode: "human" })],
    chain: ["https://jira.example.org/login"],
  });

  await act(async () => {
    fireEvent.click(screen.getByText("Give the wheel back"));
  });
  await act(async () => {
    fireEvent.click(screen.getByText("Keep none"));
  });

  const kept = fetchMock.mock.calls.find((call) => String(call[0]).includes("/browser/keep"));
  expect(JSON.parse(String(kept?.[1].body)).keep).toBe(false);
});

/**
 * The way back, which is the counterweight to a list that only grows by a human act. Both halves are
 * here on purpose: revoking stops a host loading again, and forgetting removes the cookies it left.
 */
it("can take back one host and forget the whole profile", async () => {
  await show({
    sites: [
      {
        origin: "https://jira.example.org:443",
        kind: "destination",
        granted_at: "2026-08-10T10:00:00Z",
        granted_for: null,
      },
      {
        origin: "https://accounts.google.com:443",
        kind: "idp",
        granted_at: "2026-08-10T10:00:00Z",
        granted_for: "https://jira.example.org:443",
      },
    ],
  });

  await act(async () => {
    fireEvent.change(screen.getByLabelText("Project id"), { target: { value: "acme" } });
  });
  await act(async () => {
    fireEvent.click(screen.getByText("Show"));
  });

  expect(screen.getByText("https://jira.example.org:443")).toBeTruthy();
  expect(screen.getByText("signed in through this")).toBeTruthy();
  expect(screen.getByText("logged in here")).toBeTruthy();

  // Both are two-click confirmations: they take away access that was granted deliberately.
  const forget = screen.getByText("Forget this profile");
  await act(async () => {
    fireEvent.click(forget);
  });
  expect(fetchMock.mock.calls.some((call) => String(call[0]).includes("/browser/forget"))).toBe(
    false,
  );
});

/** A session the fence refused exists and is empty — a value, not a failure, and it says so. */
it("says when the fence refused the page a session was opened for", async () => {
  await show({ sessions: [session({ refusal: "off-allowlist" })] });

  expect(screen.getByText(/The fence stopped this page/)).toBeTruthy();
  expect(screen.getByText("off-allowlist")).toBeTruthy();
});
