import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

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

/** The readout for `/channels`, and an empty identities body for each channel's own read. */
function serve(data: ChannelsReadout) {
  daemon.apiFetch.mockImplementation(async (path: string) =>
    /\/identities$/.test(path) ? { channel: path.split("/")[2], home: null, identities: [], projects: [] } : data,
  );
}

const channelButtons = async () =>
  within(await screen.findByRole("list", { name: "Channel list" })).getAllByRole("button");

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("ChannelsView", () => {
  it("lists every channel in the núcleo's order, with the primary marker on the primary only", async () => {
    serve(readout());
    await renderWithRouter(<ChannelsView />);
    const buttons = await channelButtons();
    expect(buttons.map((b) => b.textContent)).toEqual(["Telegramonlineprimary", "Discordoffline"]);
    expect(screen.getAllByText("primary")).toHaveLength(1);
  });

  it("opens the primary channel's detail first, and the first channel when there is no primary", async () => {
    serve(readout());
    const first = await renderWithRouter(<ChannelsView />);
    expect(await screen.findByRole("heading", { level: 2, name: "Telegram" })).toBeTruthy();
    expect((await channelButtons())[0]!.getAttribute("aria-current")).toBe("true");
    first.unmount();

    serve(readout({ primary: "discord" }));
    const second = await renderWithRouter(<ChannelsView />);
    expect(await screen.findByRole("heading", { level: 2, name: "Discord" })).toBeTruthy();
    second.unmount();

    serve(readout({ primary: null }));
    await renderWithRouter(<ChannelsView />);
    expect(await screen.findByRole("heading", { level: 2, name: "Telegram" })).toBeTruthy();
  });

  it("shows the detail of the row that is pressed, and marks that row current", async () => {
    serve(readout());
    await renderWithRouter(<ChannelsView />);
    const buttons = await channelButtons();
    fireEvent.click(buttons[1]!);
    expect(await screen.findByRole("heading", { level: 2, name: "Discord" })).toBeTruthy();
    expect(screen.queryByRole("heading", { level: 2, name: "Telegram" })).toBeNull();
    const after = await channelButtons();
    expect(after[1]!.getAttribute("aria-current")).toBe("true");
    expect(after[0]!.getAttribute("aria-current")).toBeNull();
    expect(screen.getByRole("button", { name: "Make primary" })).toBeTruthy();
  });

  it("lists the running configurations after the channels and shows their detail when selected", async () => {
    serve(readout());
    const configs = [
      { id: "email", label: "Email", badge: <span>armed</span>, detail: <p>mailbox facts</p> },
      { id: "calendar", label: "Calendar", detail: <p>calendar facts</p> },
    ];
    await renderWithRouter(<ChannelsView configs={configs} />);
    const buttons = await channelButtons();
    expect(buttons.map((b) => b.textContent)).toEqual(["Telegramonlineprimary", "Discordoffline", "Emailarmed", "Calendar"]);
    expect(screen.queryByText("mailbox facts")).toBeNull();
    fireEvent.click(buttons[2]!);
    expect(await screen.findByText("mailbox facts")).toBeTruthy();
    expect(screen.getByRole("heading", { level: 2, name: "Email" })).toBeTruthy();
  });

  it.each<[RoutingState, RegExp]>([
    ["no_primary", /no primary channel/i],
    ["primary_not_configured", /primary channel is not configured/i],
    ["primary_without_home", /primary channel has no home/i],
  ])("warns about routing state %s", async (state, sentence) => {
    serve(readout({ routing: { state } }));
    await renderWithRouter(<ChannelsView />);
    // The sentence is a tip beside the list heading, not a paragraph of the page.
    expect(await screen.findByText(sentence)).toBeTruthy();
  });

  it("says nothing about routing when it is ok", async () => {
    serve(readout());
    await renderWithRouter(<ChannelsView />);
    await screen.findByRole("heading", { level: 2, name: "Telegram" });
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

  it("keeps the running configurations listed when the channels cannot be read", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("down"));
    await renderWithRouter(<ChannelsView configs={[{ id: "email", label: "Email", detail: <p>mailbox facts</p> }]} />);
    expect(await screen.findByRole("button", { name: "Retry" })).toBeTruthy();
    expect(screen.getByRole("heading", { level: 2, name: "Email" })).toBeTruthy();
    expect(screen.getByText("mailbox facts")).toBeTruthy();
  });

  it("offers Retry on a network failure and refetches", async () => {
    daemon.apiFetch.mockRejectedValueOnce(new Error("down"));
    daemon.apiFetch.mockImplementation(async (path: string) =>
      /\/identities$/.test(path) ? { channel: "telegram", home: null, identities: [], projects: [] } : readout(),
    );
    await renderWithRouter(<ChannelsView />);
    fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("heading", { level: 2, name: "Telegram" })).toBeTruthy();
  });
});
