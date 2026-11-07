import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const opener = vi.hoisted(() => ({ openUrl: vi.fn(async () => undefined) }));
vi.mock("@tauri-apps/plugin-opener", () => opener);

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

import { AccountsView, defaultOptions, readingText } from "./AccountsView";
import type { PoolAccount } from "../../data/accounts";
import { renderWithQuery } from "../../test/harness";

function account(over: Partial<PoolAccount> = {}): PoolAccount {
  return {
    id: "claude:work",
    engine: "claude",
    provider: "anthropic",
    label: "Work",
    kind: "subscription",
    state: "active",
    is_default: false,
    created_at: "",
    updated_at: "",
    live_sessions: 2,
    usage: {
      source: "official",
      read_at: new Date().toISOString(),
      windows: [
        { name: "5h", used_fraction: 0.42, resets_at: null, stale: false },
        { name: "7d", used_fraction: 0.1, resets_at: null, stale: false },
      ],
    },
    ...over,
  };
}

const calls: { path: string; method: string; body?: string }[] = [];

function serve(accounts: PoolAccount[]) {
  calls.length = 0;
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    calls.push({ path, method, body: init?.body as string | undefined });
    if (path === "/accounts" && method === "GET") return { accounts };
    if (path === "/account-pool/default") return { choice: "auto" };
    if (path === "/accounts" && method === "POST") {
      return { login_id: "l1", account_id: "claude:new", engine: "claude", label: "New", phase: "awaiting_browser", auth_url: "https://auth.example/x", message: null };
    }
    if (path === "/account-logins/l1" && method === "GET") {
      return { login_id: "l1", account_id: "claude:new", engine: "claude", label: "New", phase: "awaiting_code", auth_url: "https://auth.example/x", message: null };
    }
    if (path === "/account-logins/l1/code") {
      return { login_id: "l1", account_id: "claude:new", engine: "claude", label: "New", phase: "verifying", auth_url: null, message: null };
    }
    return {};
  });
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  opener.openUrl.mockClear();
});

describe("the accounts view", () => {
  it("lists accounts with bars, sessions and reading, plus a disabled API key row", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    expect(await screen.findByText("Work")).toBeTruthy();
    expect(screen.getByText("42%")).toBeTruthy();
    expect(screen.getByText("10%")).toBeTruthy();
    expect(screen.getByText("2")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Add" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("saves the default choice", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.change(screen.getByLabelText("Default account"), { target: { value: "account:claude:work" } });
    await waitFor(() =>
      expect(calls.some((c) => c.method === "PUT" && c.body === JSON.stringify({ choice: "account:claude:work" }))).toBe(true),
    );
  });

  it("pauses an account through the menu, with the id URL-encoded", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "Actions for Work" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Pause" }));
    await waitFor(() => expect(calls.some((c) => c.path === "/accounts/claude%3Awork/pause" && c.method === "POST")).toBe(true));
  });

  it("asks before removing an account with live sessions, then forces", async () => {
    serve([account()]);
    const { ApiRefusal } = await import("../../data/client");
    const base = daemon.apiFetch.getMockImplementation()!;
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method === "DELETE" && !path.includes("force")) {
        throw new ApiRefusal(409, "live_sessions", "", { refusal: "live_sessions", live_sessions: 2 });
      }
      return base(path, init);
    });
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "Actions for Work" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Remove" }));
    fireEvent.click(await screen.findByRole("button", { name: "Remove anyway" }));
    await waitFor(() => expect(calls.some((c) => c.path === "/accounts/claude%3Awork?force=true")).toBe(true));
  });

  it("adds an account: shows and opens the auth URL, then submits the pasted code", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "+ Add account" }));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "New" } });
    fireEvent.click(screen.getByRole("button", { name: "Start login" }));
    expect(await screen.findByText("https://auth.example/x")).toBeTruthy();
    await waitFor(() => expect(opener.openUrl).toHaveBeenCalledWith("https://auth.example/x"));
    const field = await screen.findByLabelText("Paste code");
    fireEvent.change(field, { target: { value: "abc123" } });
    fireEvent.click(screen.getByRole("button", { name: "Submit code" }));
    await waitFor(() =>
      expect(calls.some((c) => c.path === "/account-logins/l1/code" && c.body === JSON.stringify({ code: "abc123" }))).toBe(true),
    );
  });
});

describe("helpers", () => {
  it("builds the default options: Auto, providers, accounts", () => {
    expect(defaultOptions([account()]).map((o) => o.value)).toEqual(["auto", "provider:anthropic", "account:claude:work"]);
  });

  it("reads the reading source and age, or says not measured", () => {
    const now = Date.parse("2026-11-07T12:10:00Z");
    expect(readingText(account({ usage: { source: "official", read_at: "2026-11-07T12:07:00Z", windows: [] } }), now)).toBe("official, 3 min ago");
    expect(readingText(account({ usage: null }), now)).toBe("not measured");
  });
});
