import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { AccountChip, chipText, resetsIn, usageLine } from "./AccountChip";
import type { PoolAccount } from "../data/accounts";
import { renderWithQuery } from "../test/harness";

const NOW = Date.parse("2026-11-07T10:00:00Z");

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
    live_sessions: 0,
    usage: {
      source: "official",
      read_at: null,
      windows: [
        { name: "5h", used_fraction: 0.42, resets_at: "2026-11-07T12:05:00Z", stale: false },
        { name: "7d", used_fraction: 0.1, resets_at: null, stale: false },
      ],
    },
    ...over,
  };
}

const calls: { path: string; method: string; body?: string }[] = [];

function serve(choice: string, current: string | null) {
  calls.length = 0;
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    calls.push({ path, method, body: init?.body as string | undefined });
    if (path === "/accounts") return { accounts: [account()] };
    if (path === "/assistant/chats/c1/account" && method === "GET") {
      return {
        chat_id: "c1",
        choice,
        inherited: false,
        current_account_id: current,
        previous_account_id: null,
        switched_at: null,
      };
    }
    return { chat_id: "c1", choice };
  });
}

beforeEach(() => daemon.apiFetch.mockReset());

describe("account chip text", () => {
  it("formats the reset countdown and the usage line", () => {
    expect(resetsIn("2026-11-07T12:05:00Z", NOW)).toBe("2h 5m");
    expect(resetsIn("2026-11-07T09:00:00Z", NOW)).toBeNull();
    expect(usageLine(account(), NOW)).toBe("5h 42% \u00b7 7d 10% \u00b7 resets in 2h 5m");
  });

  it("shows Auto with the current account, or the pinned one", () => {
    expect(chipText("auto", "Work", [account()])).toBe("Auto \u2192 Work");
    expect(chipText("auto", null, [account()])).toBe("Auto");
    expect(chipText("account:claude:work", null, [account()])).toBe("Work");
    expect(chipText("provider:anthropic", null, [account()])).toBe("Any anthropic");
  });
});

describe("the account chip", () => {
  it("shows Auto -> current account", async () => {
    serve("auto", "claude:work");
    renderWithQuery(<AccountChip chatId="c1" />);
    expect(await screen.findByText("Auto \u2192 Work")).toBeTruthy();
  });

  it("pins the chat with a PUT when an account is picked", async () => {
    serve("auto", "claude:work");
    renderWithQuery(<AccountChip chatId="c1" />);
    const trigger = await screen.findByRole("button", { name: /Change the account/ });
    fireEvent.keyDown(trigger, { key: "Enter" });
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /Work/ }));
    await waitFor(() =>
      expect(
        calls.some(
          (c) =>
            c.method === "PUT" &&
            c.path === "/assistant/chats/c1/account" &&
            c.body === JSON.stringify({ choice: "account:claude:work" }),
        ),
      ).toBe(true),
    );
  });
});
