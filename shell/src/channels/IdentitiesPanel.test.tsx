import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

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

  describe("actions", () => {
    const twoSenders: ChannelIdentities = {
      ...example,
      identities: [
        example.identities[0]!,
        {
          sender_ref: "777", sender_name: "Ana", role: "member",
          conversations: [
            { conversation_ref: "777", kind: "private", parent: null, home: false, covers_threads: false, created_at: "2026-10-14T09:15:40Z" },
          ],
        },
      ],
    };
    const single = (ref: string, home: boolean): ChannelIdentities => ({
      ...example,
      home: home ? ref : null,
      identities: [
        {
          ...example.identities[0]!,
          conversations: [
            { conversation_ref: ref, kind: "private", parent: null, home, covers_threads: false, created_at: "2026-10-14T09:15:40Z" },
          ],
        },
      ],
    });

    function route(data: ChannelIdentities, reply: (path: string, init: RequestInit) => unknown) {
      daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
        if (init?.method === undefined) return data;
        const out = reply(path, init);
        if (out instanceof Error) throw out;
        return out;
      });
    }
    const mutations = () => daemon.apiFetch.mock.calls.filter((c) => c[1]?.method !== undefined);
    const row = async (ref: string) => {
      await screen.findByText("Duarte");
      return screen.getAllByTestId("identity-conversation").find((r) => r.getAttribute("data-ref") === ref)!;
    };
    const NO_OWNER =
      "this channel will have no owner: nobody will be able to talk to the bot until someone is paired again";

    it("offers Make home on every conversation but the home, and sends the PUT", async () => {
      route(example, () => ({ home: "4242", previous: "-100123:5" }));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      expect(within(await row("-100123:5")).queryByRole("button", { name: "Make home" })).toBeNull();
      fireEvent.click(within(await row("4242")).getByRole("button", { name: "Make home" }));

      await waitFor(() => expect(mutations()).toHaveLength(1));
      const [path, init] = mutations()[0]!;
      expect(path).toBe("/channels/telegram/identities/4242/home");
      expect(init.method).toBe("PUT");
      expect(JSON.parse(init.body)).toEqual({ conversation_ref: "4242" });
      // Success invalidates the identities, so they are read again.
      await waitFor(() => expect(daemon.apiFetch.mock.calls.filter((c) => c[1]?.method === undefined).length).toBeGreaterThan(1));
    });

    it("explains unknown_conversation on Make home", async () => {
      route(example, () => new ApiRefusal(404, "unknown_conversation", ""));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      fireEvent.click(within(await row("4242")).getByRole("button", { name: "Make home" }));
      expect(await screen.findByText("that conversation is no longer listed for this sender")).toBeTruthy();
    });

    it("removes a conversation only after the confirmation, with the ref encoded", async () => {
      route(twoSenders, () => ({ removed: 1, home_cleared: false }));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      fireEvent.click(within(await row("-100123:5")).getByRole("button", { name: "Remove conversation" }));
      await screen.findByRole("button", { name: "Remove" });
      expect(mutations()).toHaveLength(0);
      fireEvent.click(screen.getByRole("button", { name: "Remove" }));

      await waitFor(() => expect(mutations()).toHaveLength(1));
      const [path, init] = mutations()[0]!;
      expect(path).toBe("/channels/telegram/identities/4242?conversation=-100123%3A5");
      expect(init.method).toBe("DELETE");
      expect(await screen.findByText("removed 1")).toBeTruthy();
    });

    it("warns that removing the home leaves no home, and for the primary that notifications stop", async () => {
      route(twoSenders, () => ({ removed: 1, home_cleared: true }));
      const first = await renderWithRouter(<IdentitiesPanel channel={channel} />);
      fireEvent.click(within(await row("-100123:5")).getByRole("button", { name: "Remove conversation" }));
      expect(await screen.findByText("this channel will have no home conversation")).toBeTruthy();
      expect(screen.queryByText("notifications will stop until you pick a new home")).toBeNull();
      expect(screen.queryByText(/this channel will have no owner/)).toBeNull();
      first.unmount();

      await renderWithRouter(<IdentitiesPanel channel={channel} primary />);
      fireEvent.click(within(await row("-100123:5")).getByRole("button", { name: "Remove conversation" }));
      expect(await screen.findByText("notifications will stop until you pick a new home")).toBeTruthy();
    });

    it("does not warn about the home when the conversation is not the home", async () => {
      route(twoSenders, () => ({ removed: 1, home_cleared: false }));
      await renderWithRouter(<IdentitiesPanel channel={channel} primary />);
      fireEvent.click(within(await row("4242")).getByRole("button", { name: "Remove conversation" }));
      await screen.findByRole("button", { name: "Remove" });
      expect(screen.queryByText(/no home conversation/)).toBeNull();
    });

    it("removes a sender with no query and reports a cleared home", async () => {
      route(twoSenders, () => ({ removed: 2, home_cleared: true }));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      await screen.findByText("Duarte");
      fireEvent.click(screen.getAllByRole("button", { name: "Remove sender" })[0]!);
      await screen.findByRole("button", { name: "Remove" });
      expect(mutations()).toHaveLength(0);
      fireEvent.click(screen.getByRole("button", { name: "Remove" }));

      await waitFor(() => expect(mutations()).toHaveLength(1));
      expect(mutations()[0]![0]).toBe("/channels/telegram/identities/4242");
      expect(mutations()[0]![1].method).toBe("DELETE");
      expect(await screen.findByText("removed 2, the home conversation was cleared")).toBeTruthy();
    });

    it("warns of no owner when the last sender is removed", async () => {
      route(example, () => ({ removed: 3, home_cleared: true }));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      await screen.findByText("Duarte");
      fireEvent.click(screen.getByRole("button", { name: "Remove sender" }));
      expect(await screen.findByText(NO_OWNER)).toBeTruthy();
      expect(screen.getByText("this channel will have no home conversation")).toBeTruthy();
    });

    it("warns of no owner when the last conversation of the last sender is removed", async () => {
      route(single("4242", false), () => ({ removed: 1, home_cleared: false }));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      await screen.findByText("Duarte");
      fireEvent.click(screen.getByRole("button", { name: "Remove conversation" }));
      expect(await screen.findByText(NO_OWNER)).toBeTruthy();
      expect(screen.queryByText(/no home conversation/)).toBeNull();
    });

    it("does not warn of no owner when another conversation or sender remains", async () => {
      route(example, () => ({ removed: 1, home_cleared: false }));
      const first = await renderWithRouter(<IdentitiesPanel channel={channel} />);
      fireEvent.click(within(await row("4242")).getByRole("button", { name: "Remove conversation" }));
      await screen.findByRole("button", { name: "Remove" });
      expect(screen.queryByText(NO_OWNER)).toBeNull();
      first.unmount();

      route(twoSenders, () => ({ removed: 3, home_cleared: true }));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      await screen.findByText("Duarte");
      fireEvent.click(screen.getAllByRole("button", { name: "Remove sender" })[0]!);
      await screen.findByRole("button", { name: "Remove" });
      expect(screen.queryByText(NO_OWNER)).toBeNull();
    });

    it("sends nothing when the confirmation is cancelled", async () => {
      route(twoSenders, () => ({ removed: 1, home_cleared: false }));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      fireEvent.click(within(await row("4242")).getByRole("button", { name: "Remove conversation" }));
      fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
      await waitFor(() => expect(screen.queryByRole("button", { name: "Remove" })).toBeNull());
      expect(mutations()).toHaveLength(0);
    });

    it("explains unknown_identity and malformed_ref in the confirmation", async () => {
      route(twoSenders, () => new ApiRefusal(404, "unknown_identity", ""));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      await screen.findByText("Duarte");
      fireEvent.click(screen.getAllByRole("button", { name: "Remove sender" })[0]!);
      fireEvent.click(await screen.findByRole("button", { name: "Remove" }));
      expect(await screen.findByText("that sender is no longer paired on this channel")).toBeTruthy();

      route(twoSenders, () => new ApiRefusal(400, "malformed_ref", ""));
      fireEvent.click(screen.getByRole("button", { name: "Remove" }));
      expect(await screen.findByText("that conversation reference is not valid")).toBeTruthy();
    });

    it("disables the buttons while a change is pending", async () => {
      daemon.apiFetch.mockImplementation((_p: string, init?: RequestInit) =>
        init?.method === undefined ? Promise.resolve(twoSenders) : new Promise(() => {}),
      );
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      fireEvent.click(within(await row("4242")).getByRole("button", { name: "Make home" }));
      await waitFor(() =>
        expect((screen.getAllByRole("button", { name: "Make home" })[0] as HTMLButtonElement).disabled).toBe(true),
      );
      expect((screen.getAllByRole("button", { name: "Remove sender" })[0] as HTMLButtonElement).disabled).toBe(true);
    });

    it("shows a network error in the confirmation", async () => {
      route(twoSenders, () => new Error("down"));
      await renderWithRouter(<IdentitiesPanel channel={channel} />);
      await screen.findByText("Duarte");
      fireEvent.click(screen.getAllByRole("button", { name: "Remove sender" })[0]!);
      fireEvent.click(await screen.findByRole("button", { name: "Remove" }));
      expect(await screen.findByRole("alert")).toBeTruthy();
    });
  });
});
