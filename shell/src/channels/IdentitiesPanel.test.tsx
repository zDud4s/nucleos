import { beforeEach, describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import type { Channel, ChannelIdentities } from "../data/channels";
import { renderWithRouter } from "../test/harness";
import { IdentitiesPanel } from "./IdentitiesPanel";

const channel = { id: "telegram", label: "Telegram", state: "online", configured: true } as unknown as Channel;

// The example of annex E.5.
const example: ChannelIdentities = {
  channel: "telegram",
  home: "-100123:5",
  identities: [
    {
      sender_ref: "4242", sender_name: "Duarte", role: "owner",
      conversations: [
        { conversation_ref: "-100123", kind: "group", parent: null, home: false, covers_threads: true, created_at: "2026-10-12T18:02:11Z" },
        { conversation_ref: "-100123:5", kind: "thread", parent: "-100123", home: true, covers_threads: false, created_at: "2026-10-12T18:02:11Z" },
        { conversation_ref: "4242", kind: "private", parent: null, home: false, covers_threads: false, created_at: "2026-10-14T09:15:40Z" },
      ],
    },
  ],
  projects: [{ project_id: "alpha", conversation_ref: "-100123:7" }],
};

function answer(body: ChannelIdentities | Error) {
  daemon.apiFetch.mockImplementation(async () => {
    if (body instanceof Error) throw body;
    return body;
  });
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

describe("IdentitiesPanel", () => {
  it("draws the sender, the conversations in the daemon's order, the home and all topics", async () => {
    answer(example);
    await renderWithRouter(<IdentitiesPanel channel={channel} />);

    expect(await screen.findByText("Duarte")).toBeTruthy();
    expect(screen.getByText("4242", { selector: ".identities-sender-ref" })).toBeTruthy();
    const rows = screen.getAllByTestId("identity-conversation");
    expect(rows.map((r) => r.getAttribute("data-ref"))).toEqual(["-100123", "-100123:5", "4242"]);
    expect(within(rows[0]!).getByText("group")).toBeTruthy();
    expect(within(rows[1]!).getByText("thread")).toBeTruthy();
    expect(within(rows[2]!).getByText("private")).toBeTruthy();

    expect(screen.getAllByText("home")).toHaveLength(1);
    expect(within(rows[1]!).getByText("home")).toBeTruthy();
    expect(screen.getAllByText("all topics")).toHaveLength(1);
    expect(within(rows[0]!).getByText("all topics")).toBeTruthy();
    expect(daemon.apiFetch.mock.calls[0]![0]).toBe("/channels/telegram/identities");
  });

  it("lists the projects read-only with a link to Machine settings", async () => {
    answer(example);
    await renderWithRouter(<IdentitiesPanel channel={channel} />);

    expect(await screen.findByText("alpha")).toBeTruthy();
    expect(screen.getByText("-100123:7")).toBeTruthy();
    const link = screen.getByRole("link", { name: /edit in Machine/i });
    expect(link.getAttribute("href")).toMatch(/\/system\/settings$/);
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  it("falls back to the sender ref when the name is null", async () => {
    const nameless: ChannelIdentities = {
      ...example,
      identities: [{ ...example.identities[0]!, sender_name: null }],
    };
    answer(nameless);
    await renderWithRouter(<IdentitiesPanel channel={channel} />);

    expect(await screen.findByText("4242", { selector: ".identities-sender-name" })).toBeTruthy();
    expect(screen.queryByText("Duarte")).toBeNull();
  });

  it("says nobody is paired, and still shows the projects", async () => {
    answer({ ...example, home: null, identities: [] });
    await renderWithRouter(<IdentitiesPanel channel={channel} />);

    expect(await screen.findByText("nobody is paired on this channel yet")).toBeTruthy();
    expect(screen.getByText("alpha")).toBeTruthy();
  });

  it("shows a loading status while the identities are read", async () => {
    daemon.apiFetch.mockImplementation(() => new Promise(() => {}));
    await renderWithRouter(<IdentitiesPanel channel={channel} />);
    expect(screen.getByRole("status").textContent).toMatch(/reading the identities/);
  });

  it("explains a refusal", async () => {
    answer(new ApiRefusal(404, "unknown_channel", ""));
    await renderWithRouter(<IdentitiesPanel channel={channel} />);
    expect(await screen.findByText("unknown_channel")).toBeTruthy();
  });

  it("offers a retry when the daemon cannot be reached", async () => {
    answer(new Error("down"));
    await renderWithRouter(<IdentitiesPanel channel={channel} />);
    expect(await screen.findByRole("button", { name: "Retry" })).toBeTruthy();
  });

  it("shows the identities of a channel that is not configured", async () => {
    answer(example);
    const gone = { ...channel, state: "not_configured", configured: false } as Channel;
    await renderWithRouter(<IdentitiesPanel channel={gone} />);
    expect(await screen.findByText("Duarte")).toBeTruthy();
  });
});
