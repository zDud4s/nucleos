import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  apiFetch: daemon.apiFetch,
}));

import { ApiRefusal } from "../data/client";
import type { Channel } from "../data/channels";
import { renderWithQuery } from "../test/harness";
import { PrimaryChannelControl } from "./PrimaryChannelControl";

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function channel(overrides: Partial<Channel> = {}): Channel {
  return { id: "discord", kind: "discord", label: "Discord", state: "online", ...overrides } as Channel;
}

const afterDwell = () => new Promise<void>((resolve) => setTimeout(resolve, 350));

function putCalls() {
  return daemon.apiFetch.mock.calls.filter(([path]) => path === "/channels/primary");
}

describe("PrimaryChannelControl", () => {
  it("makes a non-primary channel primary", async () => {
    daemon.apiFetch.mockResolvedValue({ primary: "discord", reseeded: false });
    renderWithQuery(<PrimaryChannelControl channel={channel()} primary="telegram" />);
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    await waitFor(() => expect(putCalls()).toHaveLength(1));
    expect(putCalls()[0]).toEqual(["/channels/primary", { method: "PUT", body: JSON.stringify({ channel: "discord" }) }]);
  });

  it("disables the button while the request is in flight", async () => {
    daemon.apiFetch.mockReturnValue(new Promise(() => {}));
    renderWithQuery(<PrimaryChannelControl channel={channel()} primary={null} />);
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    await waitFor(() =>
      expect((screen.getByRole("button", { name: "Make primary" }) as HTMLButtonElement).disabled).toBe(true),
    );
  });

  it("shows the badge on the primary and clears it only after confirmation", async () => {
    daemon.apiFetch.mockResolvedValue({ primary: null, reseeded: false });
    renderWithQuery(<PrimaryChannelControl channel={channel()} primary="discord" />);
    expect(screen.getByText("primary")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Clear primary" }));
    expect(putCalls()).toHaveLength(0);
    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: "Clear: notifications will have no destination" }));
    await waitFor(() => expect(putCalls()).toHaveLength(1));
    expect(putCalls()[0][1]).toEqual({ method: "PUT", body: JSON.stringify({ channel: null }) });
  });

  it("says a reseeded channel starts from now, and says nothing otherwise", async () => {
    daemon.apiFetch.mockResolvedValue({ primary: "discord", reseeded: true });
    const first = renderWithQuery(<PrimaryChannelControl channel={channel()} primary={null} />);
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    expect(await screen.findByText("new channel: it starts from now, without replaying what it missed")).toBeTruthy();
    first.unmount();

    daemon.apiFetch.mockResolvedValue({ primary: "discord", reseeded: false });
    renderWithQuery(<PrimaryChannelControl channel={channel()} primary={null} />);
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    await waitFor(() => expect(putCalls()).toHaveLength(2));
    expect(screen.queryByText(/starts from now/)).toBeNull();
  });

  it("explains no_home_conversation and channel_not_configured", async () => {
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(409, "no_home_conversation", ""));
    const first = renderWithQuery(<PrimaryChannelControl channel={channel()} primary={null} />);
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    expect(
      await screen.findByText(/pair a conversation on this channel first: notifications would have nowhere to go/),
    ).toBeTruthy();
    first.unmount();

    daemon.apiFetch.mockRejectedValue(new ApiRefusal(409, "channel_not_configured", ""));
    renderWithQuery(<PrimaryChannelControl channel={channel()} primary={null} />);
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    expect(await screen.findByText(/this channel is missing its secrets: add them in Machine settings/)).toBeTruthy();
  });

  it("falls back to the generic refusal sentence for an unknown refusal", async () => {
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(409, "conflict", ""));
    renderWithQuery(<PrimaryChannelControl channel={channel()} primary={null} />);
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    expect(await screen.findByText(/something about this has already changed/)).toBeTruthy();
  });

  it("shows an ErrorNote on a network error", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("down"));
    renderWithQuery(<PrimaryChannelControl channel={channel()} primary={null} />);
    fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
    expect(await screen.findByRole("alert")).toBeTruthy();
  });

  it("lets an offline channel be primary and says the outbox keeps the notifications", () => {
    renderWithQuery(<PrimaryChannelControl channel={channel({ state: "offline" })} primary={null} />);
    expect((screen.getByRole("button", { name: "Make primary" }) as HTMLButtonElement).disabled).toBe(false);
    expect(
      screen.getByText("the outbox keeps the notifications and delivers them when the adapter is back"),
    ).toBeTruthy();
  });
});
