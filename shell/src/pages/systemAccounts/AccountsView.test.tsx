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

function serve(accounts: PoolAccount[], engines?: string[]) {
  calls.length = 0;
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    calls.push({ path, method, body: init?.body as string | undefined });
    if (path === "/accounts" && method === "GET") return engines ? { accounts, engines } : { accounts };
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
  it("shows a card per account with the engine name, meters and live sessions", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    expect(await screen.findByText("Work")).toBeTruthy();
    expect(screen.getByText("Claude")).toBeTruthy();
    expect(screen.getByText("42%")).toBeTruthy();
    expect(screen.getByText("10%")).toBeTruthy();
    expect(screen.getByText("2 live sessions")).toBeTruthy();
    expect(screen.queryByText("No usage reading yet")).toBeNull();
    expect(screen.queryByRole("button", { name: "Add" })).toBeNull();
  });

  it("says there is no usage reading instead of drawing empty meters, and hides zero sessions", async () => {
    serve([account({ usage: null, live_sessions: 0 })]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    expect(screen.getByText("No usage reading yet")).toBeTruthy();
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.queryByText(/live session/)).toBeNull();
  });

  it("offers a primary Sign in on a card that needs a login", async () => {
    serve([account({ state: "needs_login" })]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(calls.some((c) => c.path === "/accounts/claude%3Awork/login" && c.method === "POST")).toBe(true));
  });

  it("offers a primary Resume on a paused card and none on an active one", async () => {
    serve([account({ state: "paused" })]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "Resume" }));
    await waitFor(() => expect(calls.some((c) => c.path === "/accounts/claude%3Awork/resume" && c.method === "POST")).toBe(true));
  });

  it("has no primary button on an active card", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    expect(screen.queryByRole("button", { name: "Sign in" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
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
    fireEvent.keyDown(screen.getByRole("button", { name: "Actions for Work" }), { key: "Enter" });
    fireEvent.click(await screen.findByRole("menuitem", { name: "Pause" }));
    await waitFor(() => expect(calls.some((c) => c.path === "/accounts/claude%3Awork/pause" && c.method === "POST")).toBe(true));
  });

  it("logs an account out through the menu, only after the confirmation", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.keyDown(screen.getByRole("button", { name: "Actions for Work" }), { key: "Enter" });
    fireEvent.click(await screen.findByRole("menuitem", { name: "Log out" }));
    expect(await screen.findByText(/Sign this account out\?/)).toBeTruthy();
    expect(calls.some((c) => c.path.endsWith("/logout"))).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Log out" }));
    await waitFor(() => expect(calls.some((c) => c.path === "/accounts/claude%3Awork/logout" && c.method === "POST")).toBe(true));
  });

  it("does not offer Log out for an account that already needs a login", async () => {
    serve([account({ state: "needs_login" })]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.keyDown(screen.getByRole("button", { name: "Actions for Work" }), { key: "Enter" });
    expect(await screen.findByRole("menuitem", { name: "Redo login" })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: "Log out" })).toBeNull();
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
    fireEvent.keyDown(screen.getByRole("button", { name: "Actions for Work" }), { key: "Enter" });
    fireEvent.click(await screen.findByRole("menuitem", { name: "Remove" }));
    fireEvent.click(await screen.findByRole("button", { name: "Remove anyway" }));
    await waitFor(() => expect(calls.some((c) => c.path === "/accounts/claude%3Awork?force=true")).toBe(true));
  });

  it("adds an account: shows and opens the auth URL, then submits the pasted code", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "+ Add account" }));
    fireEvent.change(screen.getByLabelText("Account name"), { target: { value: "New" } });
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

  it("offers the engines the daemon names as cards", async () => {
    serve([account()], ["codex", "gemini"]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "+ Add account" }));
    await waitFor(() => expect(screen.getAllByRole("radio").map((r) => r.textContent)).toEqual(["Codexcodex", "Geminigemini"]));
    expect(screen.getByRole("radio", { name: /Codex/ }).getAttribute("aria-checked")).toBe("true");
    fireEvent.change(screen.getByLabelText("Account name"), { target: { value: "New" } });
    fireEvent.click(screen.getByRole("button", { name: "Start login" }));
    await waitFor(() =>
      expect(
        calls.some((c) => c.path === "/accounts" && c.method === "POST" && c.body === JSON.stringify({ engine: "codex", label: "New" })),
      ).toBe(true),
    );
  });

  it("falls back to claude and codex when an older daemon names no engines", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "+ Add account" }));
    expect(screen.getAllByRole("radio").map((r) => r.textContent)).toEqual(["Claudeclaude", "Codexcodex"]);
  });

  it("prefills a free name and explains why Start login is disabled", async () => {
    serve([account({ label: "claude" })]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("claude");
    fireEvent.click(screen.getByRole("button", { name: "+ Add account" }));
    const name = screen.getByLabelText("Account name") as HTMLInputElement;
    expect(name.value).toBe("claude-2");
    const start = screen.getByRole("button", { name: "Start login" }) as HTMLButtonElement;
    expect(start.disabled).toBe(false);
    fireEvent.change(name, { target: { value: "" } });
    expect(start.disabled).toBe(true);
    expect(screen.getByText("Give the account a name to continue.")).toBeTruthy();
    fireEvent.change(name, { target: { value: "claude" } });
    expect(start.disabled).toBe(true);
    expect(screen.getByText(/already exists/)).toBeTruthy();
  });
});

describe("token accounts", () => {
  it("adds a Claude account from a pasted token and closes the dialog", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "+ Add account" }));
    fireEvent.click(screen.getByRole("button", { name: "Paste token" }));
    const field = screen.getByLabelText("Token") as HTMLInputElement;
    expect(field.type).toBe("password");
    const add = screen.getByRole("button", { name: "Add account" }) as HTMLButtonElement;
    expect(add.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Account name"), { target: { value: "Tok" } });
    fireEvent.change(field, { target: { value: "fake-token-value" } });
    fireEvent.click(add);
    await waitFor(() =>
      expect(
        calls.some(
          (c) =>
            c.path === "/accounts/token" &&
            c.method === "POST" &&
            c.body === JSON.stringify({ engine: "claude", label: "Tok", token: "fake-token-value" }),
        ),
      ).toBe(true),
    );
    await waitFor(() => expect(screen.queryByLabelText("Token")).toBeNull());
  });

  it("disables the token method for a non-Claude engine", async () => {
    serve([account()]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    fireEvent.click(screen.getByRole("button", { name: "+ Add account" }));
    fireEvent.click(screen.getByRole("radio", { name: /Codex/ }));
    expect((screen.getByRole("button", { name: "Paste token" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("Only Claude accounts can use a token.")).toBeTruthy();
  });

  it("replaces the token of a token account through the card", async () => {
    serve([account({ kind: "token", state: "needs_login" })]);
    renderWithQuery(<AccountsView />);
    await screen.findByText("Work");
    expect(screen.queryByRole("button", { name: "Sign in" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Replace token" }));
    const field = screen.getByLabelText("New token") as HTMLInputElement;
    expect(field.type).toBe("password");
    fireEvent.change(field, { target: { value: "fake-new-token" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(
        calls.some(
          (c) => c.path === "/accounts/claude%3Awork/token" && c.method === "POST" && c.body === JSON.stringify({ token: "fake-new-token" }),
        ),
      ).toBe(true),
    );
    expect(calls.some((c) => c.path.endsWith("/login"))).toBe(false);
  });
});

describe("helpers", () => {
  it("builds the default options: Auto, providers, accounts", () => {
    expect(defaultOptions([account()]).map((o) => o.value)).toEqual(["auto", "provider:anthropic", "account:claude:work"]);
    expect(defaultOptions([account()]).map((o) => o.label)).toEqual(["Auto", "Any Claude account", "Work (Claude)"]);
  });

  it("reads the reading source and age, or says not measured", () => {
    const now = Date.parse("2026-11-07T12:10:00Z");
    expect(readingText(account({ usage: { source: "official", read_at: "2026-11-07T12:07:00Z", windows: [] } }), now)).toBe("official, 3 min ago");
    expect(readingText(account({ usage: null }), now)).toBe("not measured");
  });
});
