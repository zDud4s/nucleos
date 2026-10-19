import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import type { Channel } from "../data/channels";
import { renderWithRouter } from "../test/harness";
import { ChannelCard } from "./ChannelCard";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({ name: "telegram", asked: true });
});

const ago = (ms: number) => new Date(Date.now() - ms).toISOString();
const HOUR = 3_600_000;

// The two channels of annex E.10, copied here so the test does not import the preview mock.
const telegram: Channel = {
  id: "telegram", kind: "telegram", label: "Telegram", health_row: "telegram_sidecar",
  configured: true, missing_secrets: [], settings_area: "channels/telegram",
  state: "online", since: ago(2 * 60_000),
  adapter: { contract_version: "1" },
  capabilities: { buttons: true, edit: true, typing: true, threads: true, reply_to: true, max_text: 4096, media_in: ["audio", "image", "file"], media_out: ["audio", "image", "file"] },
  home: { conversation_ref: "-100123:5", kind: "thread", parent: "-100123", sender_ref: "4242" },
  identities: { senders: 1, conversations: 3 },
  pairing: null,
  outbox: { pending: 0, leased: 0, failed: 1, oldest_due_at: null },
  drops: { unknown_sender: 3, pairing_failures: 0, counted_since: "2026-10-18T10:00:00Z" },
};
const discord: Channel = {
  ...telegram, id: "discord", kind: "discord", label: "Discord", health_row: "discord_sidecar",
  state: "offline", since: ago(2 * HOUR), home: null,
  capabilities: { ...telegram.capabilities!, buttons: false, edit: false, typing: false, threads: false, reply_to: false },
  outbox: { pending: 0, leased: 0, failed: 0, oldest_due_at: null },
  drops: { unknown_sender: 0, pairing_failures: 0, counted_since: "2026-10-18T10:00:00Z" },
};

const afterDwell = () => new Promise<void>((resolve) => setTimeout(resolve, 350));
const restartCalls = () => daemon.apiFetch.mock.calls.filter(([p]) => String(p).includes("/restart"));

describe("ChannelCard", () => {
  it("shows an online primary channel with contract version, counts and links", async () => {
    await renderWithRouter(<ChannelCard channel={telegram} primary="telegram" />);
    expect(screen.getByText("online")).toBeTruthy();
    expect(screen.getByText("2min ago")).toBeTruthy();
    expect(screen.getByText(/contract 1/)).toBeTruthy();
    expect(screen.queryByText(/adapter_version/)).toBeNull();
    expect(screen.queryByText(/adapter version/i)).toBeNull();
    expect(screen.getByText("primary")).toBeTruthy();
    expect(screen.getByText(/1 failed/)).toBeTruthy();
    expect(screen.getByText(/3 from unknown senders/)).toBeTruthy();
    const link = screen.getByRole("link", { name: /health/i });
    expect(link.getAttribute("href")).toBe("/system/health");
  });

  it("restarts the sidecar behind a confirmation", async () => {
    await renderWithRouter(<ChannelCard channel={telegram} primary="telegram" />);
    fireEvent.click(screen.getByRole("button", { name: "Restart" }));
    expect(restartCalls()).toHaveLength(0);
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Start it again" }));
    await waitFor(() => expect(restartCalls()).toHaveLength(1));
    expect(restartCalls()[0]).toEqual(["/sidecars/telegram/restart", { method: "POST" }]);
  });

  it("shows failed pairing attempts only when there are some", async () => {
    const first = await renderWithRouter(
      <ChannelCard channel={{ ...telegram, drops: { ...telegram.drops, pairing_failures: 4 } }} primary={null} />,
    );
    expect(screen.getByText("4 failed pairing attempts")).toBeTruthy();
    first.unmount();
    await renderWithRouter(<ChannelCard channel={telegram} primary={null} />);
    expect(screen.queryByText(/failed pairing attempts/)).toBeNull();
  });

  it("shows the oldest waiting message only with pending messages and a due date", async () => {
    const waiting = { pending: 2, leased: 0, failed: 0, oldest_due_at: ago(3 * HOUR) };
    const first = await renderWithRouter(<ChannelCard channel={{ ...telegram, outbox: waiting }} primary={null} />);
    expect(screen.getByText(/oldest waiting/)).toBeTruthy();
    expect(screen.getByText("3h ago")).toBeTruthy();
    first.unmount();
    const second = await renderWithRouter(
      <ChannelCard channel={{ ...telegram, outbox: { ...waiting, pending: 0 } }} primary={null} />,
    );
    expect(screen.queryByText(/oldest waiting/)).toBeNull();
    second.unmount();
    await renderWithRouter(
      <ChannelCard channel={{ ...telegram, outbox: { ...waiting, oldest_due_at: null } }} primary={null} />,
    );
    expect(screen.queryByText(/oldest waiting/)).toBeNull();
  });

  it("omits Restart for a health row with no sidecar key but keeps the health link", async () => {
    await renderWithRouter(<ChannelCard channel={{ ...telegram, health_row: "weird_row" }} primary={null} />);
    expect(screen.queryByRole("button", { name: "Restart" })).toBeNull();
    expect(screen.getByRole("link", { name: /health/i }).getAttribute("href")).toBe("/system/health");
  });

  it("shows an offline channel with the lacking capabilities and Make primary", async () => {
    await renderWithRouter(<ChannelCard channel={discord} primary="telegram" />);
    expect(screen.getByText("offline")).toBeTruthy();
    expect(screen.getByText("2h ago")).toBeTruthy();
    expect(screen.getByText("buttons")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Make primary" })).toBeTruthy();
    expect(screen.queryByText("primary")).toBeNull();
  });

  it("gives every state a label and a sentence", async () => {
    const cases: [Channel["state"], string, RegExp][] = [
      ["starting", "starting", /waiting for the adapter/i],
      ["online", "online", /connected and delivering/i],
      ["offline", "offline", /not connected right now/i],
    ];
    for (const [state, label, sentence] of cases) {
      const { unmount } = await renderWithRouter(<ChannelCard channel={{ ...telegram, state }} primary={null} />);
      expect(screen.getByText(label)).toBeTruthy();
      expect(screen.getByText(sentence)).toBeTruthy();
      unmount();
    }
  });

  it("explains not_configured with the missing secrets and a settings link, without Restart or Pair", async () => {
    await renderWithRouter(
      <ChannelCard
        channel={{ ...telegram, state: "not_configured", configured: false, missing_secrets: ["telegram-token"], capabilities: null, adapter: null }}
        primary={null}
      />,
    );
    expect(screen.getByText("not configured")).toBeTruthy();
    expect(screen.getByText("telegram-token")).toBeTruthy();
    const link = screen.getByRole("link", { name: /channels\/telegram/ });
    expect(link.getAttribute("href")).toBe("/system/settings");
    expect(screen.queryByRole("button", { name: "Restart" })).toBeNull();
    expect(screen.queryByRole("button", { name: /pair/i })).toBeNull();
  });

  it("explains incompatible as a refused contract that needs an adapter update", async () => {
    await renderWithRouter(<ChannelCard channel={{ ...telegram, state: "incompatible" }} primary={null} />);
    expect(screen.getByText("incompatible")).toBeTruthy();
    expect(screen.getByText(/contract the núcleo refused/)).toBeTruthy();
    expect(screen.getByText(/update the adapter/i)).toBeTruthy();
  });

  it("does not break before the first hello (adapter and capabilities null)", async () => {
    const { container } = await renderWithRouter(
      <ChannelCard channel={{ ...telegram, state: "starting", adapter: null, capabilities: null }} primary={null} />,
    );
    expect(within(container).getByText(/not known yet/)).toBeTruthy();
    expect(screen.queryByText(/contract 1/)).toBeNull();
  });
});
