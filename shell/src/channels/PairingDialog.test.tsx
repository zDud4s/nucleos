import { StrictMode, type ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const client = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  apiFetch: client.apiFetch,
}));

import { ApiRefusal } from "../data/client";
import type { Channel } from "../data/channels";
import { PairingDialog } from "./PairingDialog";

const NOW = Date.parse("2026-10-19T10:30:00Z");
const CODE = {
  code: "482913",
  command: "/pair 482913",
  expires_at: "2026-10-19T10:40:00Z",
  attempts_left: 5,
};

function channel(over: Partial<Channel> = {}): Channel {
  return {
    id: "telegram", kind: "telegram", label: "Telegram", health_row: "telegram", configured: true,
    missing_secrets: [], settings_area: "channels/telegram", state: "online", since: null,
    adapter: { contract_version: "1" }, capabilities: null, home: null,
    identities: { senders: 0, conversations: 0 }, pairing: null,
    outbox: { pending: 0, leased: 0, failed: 0, oldest_due_at: null },
    drops: { unknown_sender: 0, pairing_failures: 0, counted_since: "2026-10-19T00:00:00Z" },
    ...over,
  };
}

function calls(method: string) {
  return client.apiFetch.mock.calls.filter(([, init]) => (init as { method?: string } | undefined)?.method === method);
}

let queryClient: QueryClient;

function mount(ui: ReactNode, strict = false) {
  queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const tree = <QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>;
  return render(strict ? <StrictMode>{tree}</StrictMode> : tree);
}

async function flush() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

const WAITING = { state: "waiting", expires_at: CODE.expires_at, attempts_left: 5 };
const PAIRED = {
  state: "paired",
  paired_at: "2026-10-19T10:31:00Z",
  identity: {
    sender_ref: "42",
    sender_name: "Duarte",
    conversation_ref: "-1001",
    conversation_kind: "group",
    parent: null,
    home: false,
  },
};

/** POST answers with `code`; each GET takes the next status of `statuses`, the last one repeating. */
function answer(code = CODE, statuses: object[] = [WAITING]) {
  let i = 0;
  client.apiFetch.mockImplementation((_path: string, init?: { method?: string }) => {
    if (init?.method === "POST") return Promise.resolve(code);
    if (init?.method === "DELETE") return Promise.resolve(undefined);
    return Promise.resolve(statuses[Math.min(i++, statuses.length - 1)]);
  });
}

async function tick(ms = 3000) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(NOW);
  client.apiFetch.mockReset();
  answer();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  localStorage.clear();
});

describe("PairingDialog", () => {
  it("asks for a code once on open and shows the command", async () => {
    mount(<PairingDialog channel={channel()} open onOpenChange={() => {}} />);
    expect(screen.getByText(/asking for a code/i)).toBeTruthy();
    await flush();
    expect(calls("POST")).toHaveLength(1);
    expect(calls("POST")[0][0]).toBe("/channels/telegram/pairing");
    expect(screen.getByText("/pair 482913")).toBeTruthy();
    expect(screen.getByText("482913")).toBeTruthy();
    expect(screen.getByText(/send this to the bot in the conversation you want to pair/i)).toBeTruthy();
  });

  it("does not post again on a re-render", async () => {
    const view = mount(<PairingDialog channel={channel()} open onOpenChange={() => {}} />);
    await flush();
    view.rerender(
      <QueryClientProvider client={queryClient}>
        <PairingDialog channel={channel({ since: "x" })} open onOpenChange={() => {}} />
      </QueryClientProvider>,
    );
    await flush();
    expect(calls("POST")).toHaveLength(1);
  });

  it("posts exactly once inside StrictMode", async () => {
    mount(<PairingDialog channel={channel()} open onOpenChange={() => {}} />, true);
    await flush();
    expect(calls("POST")).toHaveLength(1);
    expect(screen.getByText("/pair 482913")).toBeTruthy();
  });

  it("counts down and says expired from the local clock", async () => {
    mount(<PairingDialog channel={channel()} open onOpenChange={() => {}} />);
    await flush();
    expect(screen.getByText("10:00")).toBeTruthy();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(screen.getByText("09:59")).toBeTruthy();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10 * 60 * 1000);
    });
    expect(screen.getByText(/expired/i)).toBeTruthy();
    expect(calls("POST")).toHaveLength(1);
  });

  it("offers to copy the command", async () => {
    mount(<PairingDialog channel={channel()} open onOpenChange={() => {}} />);
    await flush();
    expect(screen.getByRole("button", { name: /copy the pairing command/i })).toBeTruthy();
  });

  it("notes that an offline adapter answers later, and still shows the code", async () => {
    mount(<PairingDialog channel={channel({ state: "offline" })} open onOpenChange={() => {}} />);
    await flush();
    expect(
      screen.getByText("the bot only answers when the adapter is back online; the code is valid until then"),
    ).toBeTruthy();
    expect(screen.getByText("/pair 482913")).toBeTruthy();
  });

  it("explains channel_not_configured and does not cancel on close", async () => {
    client.apiFetch.mockImplementation(() => Promise.reject(new ApiRefusal(409, "channel_not_configured", "")));
    const onOpenChange = vi.fn();
    mount(
      <PairingDialog
        channel={channel({ state: "not_configured", configured: false })}
        open
        onOpenChange={onOpenChange}
      />,
    );
    await flush();
    expect(screen.getByText(/add the channel's secrets first/i)).toBeTruthy();
    expect(screen.queryByText("482913")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await flush();
    expect(calls("DELETE")).toHaveLength(0);
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("cancels the code once when cancelled", async () => {
    const onOpenChange = vi.fn();
    mount(<PairingDialog channel={channel()} open onOpenChange={onOpenChange} />);
    await flush();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await flush();
    expect(calls("DELETE")).toHaveLength(1);
    expect(calls("DELETE")[0][0]).toBe("/channels/telegram/pairing");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("cancels the code once when closed with the X", async () => {
    const onOpenChange = vi.fn();
    mount(<PairingDialog channel={channel()} open onOpenChange={onOpenChange} />);
    await flush();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await flush();
    expect(calls("DELETE")).toHaveLength(1);
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("keeps the code out of the query cache and localStorage", async () => {
    mount(<PairingDialog channel={channel()} open onOpenChange={() => {}} />);
    await flush();
    const cached = JSON.stringify(queryClient.getQueryCache().getAll().map((q) => q.state.data));
    expect(cached).not.toContain("482913");
    expect(JSON.stringify(localStorage)).not.toContain("482913");
  });

  describe("status", () => {
    const gets = () => client.apiFetch.mock.calls.filter(([, init]) => init === undefined || (init as { method?: string }).method === undefined);
    const open = () => mount(<PairingDialog channel={channel()} open onOpenChange={() => {}} />);

    it("keeps polling while waiting and shows the attempts left", async () => {
      answer(CODE, [WAITING, { ...WAITING, attempts_left: 4 }]);
      open();
      await flush();
      expect(screen.getByText(/waiting for \/pair 482913… 5 attempts left/)).toBeTruthy();
      const before = gets().length;
      await tick();
      expect(gets().length).toBeGreaterThan(before);
      expect(screen.getByText(/waiting for \/pair 482913… 4 attempts left/)).toBeTruthy();
    });

    it("does not stop polling on none, shows no error and keeps the code", async () => {
      answer(CODE, [{ state: "none" }, WAITING]);
      open();
      await flush();
      const afterFirst = gets().length;
      expect(screen.getByText("/pair 482913")).toBeTruthy();
      await tick();
      expect(gets().length).toBeGreaterThan(afterFirst);
      expect(screen.queryByRole("alert")).toBeNull();
      expect(screen.getByText("/pair 482913")).toBeTruthy();
      expect(screen.getByText(/waiting for/)).toBeTruthy();
    });

    it("stops repeating none once the local expiry has passed", async () => {
      answer(CODE, [{ state: "none" }]);
      open();
      await flush();
      await tick(10 * 60 * 1000 + 1000);
      const settled = gets().length;
      await tick(30000);
      expect(gets().length).toBe(settled);
    });

    it("shows who, where and home on paired, invalidates and stops", async () => {
      answer(CODE, [{ ...PAIRED, identity: { ...PAIRED.identity, home: true } }]);
      open();
      const spy = vi.spyOn(queryClient, "invalidateQueries");
      await flush();
      expect(screen.getByText(/Duarte/)).toBeTruthy();
      expect(screen.getByText(/group/)).toBeTruthy();
      expect(screen.getByText(/-1001/)).toBeTruthy();
      expect(screen.getByText(/home/i)).toBeTruthy();
      const keysCalled = spy.mock.calls.map(([f]) => JSON.stringify((f as { queryKey: unknown }).queryKey));
      expect(keysCalled).toContain(JSON.stringify(["channels", "telegram", "identities"]));
      expect(keysCalled).toContain(JSON.stringify(["channels"]));
      await tick();
      const settled = gets().length;
      await tick(30000);
      expect(gets().length).toBe(settled);
    });

    it("falls back to the sender_ref and mentions the parent group", async () => {
      answer(CODE, [
        {
          ...PAIRED,
          identity: { ...PAIRED.identity, sender_name: null, conversation_kind: "thread", parent: "-1000" },
        },
      ]);
      open();
      await flush();
      expect(screen.getByText(/42/)).toBeTruthy();
      expect(screen.getByText(/and its parent group/)).toBeTruthy();
    });

    it.each([
      ["expired", { state: "expired", expires_at: CODE.expires_at }],
      ["exhausted", { state: "exhausted", attempts_left: 0 }],
      ["superseded", { state: "superseded" }],
    ])("says %s and offers a new code", async (_name, status) => {
      answer(CODE, [status]);
      open();
      await flush();
      expect(screen.getByRole("button", { name: "Get a new code" })).toBeTruthy();
      const settled = gets().length;
      await tick(30000);
      expect(gets().length).toBe(settled);
    });

    it("re-seeds after expired: a new code restarts the poll", async () => {
      answer(CODE, [WAITING, { state: "expired", expires_at: CODE.expires_at }]);
      open();
      await flush();
      await tick();
      expect(screen.getByRole("button", { name: "Get a new code" })).toBeTruthy();
      answer(
        { ...CODE, code: "111222", command: "/pair 111222" },
        [{ ...WAITING }],
      );
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Get a new code" }));
      });
      await flush();
      expect(calls("POST")).toHaveLength(2);
      expect(screen.getByText("/pair 111222")).toBeTruthy();
      expect(screen.queryByRole("button", { name: "Get a new code" })).toBeNull();
      const before = gets().length;
      await tick();
      expect(gets().length).toBeGreaterThan(before);
    });

    it("stops polling and leaves no timers when closed", async () => {
      const view = open();
      await flush();
      view.rerender(
        <QueryClientProvider client={queryClient}>
          <PairingDialog channel={channel()} open={false} onOpenChange={() => {}} />
        </QueryClientProvider>,
      );
      await flush();
      const settled = client.apiFetch.mock.calls.length;
      await tick(60000);
      expect(client.apiFetch.mock.calls.length).toBe(settled);
    });

    it("shows a note when the status request fails and keeps the code", async () => {
      client.apiFetch.mockImplementation((_path: string, init?: { method?: string }) =>
        init?.method === "POST" ? Promise.resolve(CODE) : Promise.reject(new Error("down")),
      );
      open();
      await flush();
      expect(screen.getByText(/could not be reached for the pairing status/i)).toBeTruthy();
      expect(screen.getByText("/pair 482913")).toBeTruthy();
    });
  });

  it("asks for a fresh code when reopened", async () => {
    const view = mount(<PairingDialog channel={channel()} open onOpenChange={() => {}} />);
    await flush();
    const wrap = (open: boolean) => (
      <QueryClientProvider client={queryClient}>
        <PairingDialog channel={channel()} open={open} onOpenChange={() => {}} />
      </QueryClientProvider>
    );
    view.rerender(wrap(false));
    await flush();
    answer({ ...CODE, code: "111222", command: "/pair 111222" });
    view.rerender(wrap(true));
    await flush();
    expect(calls("POST")).toHaveLength(2);
    expect(screen.queryByText("/pair 482913")).toBeNull();
    expect(screen.getByText("/pair 111222")).toBeTruthy();
  });
});
