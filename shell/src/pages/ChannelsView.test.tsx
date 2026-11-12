import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import type { Channel, ChannelsReadout, RoutingState } from "../data/channels";
import { renderWithRouter } from "../test/harness";
import { ChannelsView } from "./ChannelsView";

const telegram: Channel = {
  id: "telegram", kind: "telegram", label: "Telegram", health_row: "telegram_sidecar",
  configured: true, missing_secrets: [], settings_area: "channels/telegram",
  state: "online", since: new Date().toISOString(),
  adapter: { contract_version: "1" },
  capabilities: { buttons: true, edit: true, typing: true, threads: true, reply_to: true, max_text: 4096, media_in: [], media_out: [] },
  home: { conversation_ref: "-100123:5", kind: "thread", parent: "-100123", sender_ref: "4242" },
  identities: { senders: 1, conversations: 3 },
  pairing: null,
  outbox: { pending: 0, leased: 0, failed: 0, oldest_due_at: null },
  drops: { unknown_sender: 0, pairing_failures: 0, counted_since: "2026-10-18T10:00:00Z" },
};
const discord: Channel = {
  ...telegram, id: "discord", kind: "discord", label: "Discord", health_row: "discord_sidecar",
  state: "offline", home: null,
};

function readout(over: Partial<ChannelsReadout> = {}): ChannelsReadout {
  return {
    contract_version: "1", primary: "telegram", routing: { state: "ok" },
    channels: [telegram, discord], ...over,
  };
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("ChannelsView", () => {
  it("draws one card per channel in the núcleo's order, with the primary badge on the primary", async () => {
    daemon.apiFetch.mockResolvedValue(readout());
    await renderWithRouter(<ChannelsView />);
    const telegramHead = await screen.findByText("Telegram");
    const discordHead = screen.getByText("Discord");
    expect(telegramHead.compareDocumentPosition(discordHead) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getAllByText("primary")).toHaveLength(1);
  });

  it.each<[RoutingState, RegExp]>([
    ["no_primary", /no primary channel/i],
    ["primary_not_configured", /primary channel is not configured/i],
    ["primary_without_home", /primary channel has no home/i],
  ])("warns about routing state %s", async (state, sentence) => {
    daemon.apiFetch.mockResolvedValue(readout({ routing: { state } }));
    await renderWithRouter(<ChannelsView />);
    expect(await screen.findByText(sentence)).toBeTruthy();
  });

  it("says nothing about routing when it is ok", async () => {
    daemon.apiFetch.mockResolvedValue(readout());
    await renderWithRouter(<ChannelsView />);
    await screen.findByText("Telegram");
    expect(screen.queryByText(/no primary channel|not configured|no home/i)).toBeNull();
  });

  it("shows a status while reading", async () => {
    daemon.apiFetch.mockReturnValue(new Promise(() => {}));
    await renderWithRouter(<ChannelsView />);
    expect(screen.getByRole("status").textContent).toContain("Reading the channels");
  });

  it("says so when no adapter is built in", async () => {
    daemon.apiFetch.mockResolvedValue(readout({ channels: [], primary: null }));
    await renderWithRouter(<ChannelsView />);
    expect(await screen.findByText("no channel adapters are built into this daemon")).toBeTruthy();
  });

  it("explains a refusal", async () => {
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(423, "kill_switch", "kill_switch"));
    await renderWithRouter(<ChannelsView />);
    await waitFor(() => expect(screen.queryByText("Reading the channels")).toBeNull());
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    expect(document.body.textContent).toMatch(/kill/i);
  });

  it("offers Retry on a network failure and refetches", async () => {
    daemon.apiFetch.mockRejectedValueOnce(new Error("down"));
    daemon.apiFetch.mockResolvedValue(readout());
    await renderWithRouter(<ChannelsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Telegram")).toBeTruthy();
  });
});
