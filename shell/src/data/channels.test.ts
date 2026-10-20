import { createElement, type ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const client = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: client.apiFetch,
}));

import { keys } from "./keys";
import { POLL } from "./poll";
import {
  useCancelPairing,
  useChannelIdentities,
  useChannels,
  usePairingStatus,
  useRemoveIdentity,
  useSetHomeConversation,
  useSetPrimaryChannel,
  useStartPairing,
  type PairingStatus,
} from "./channels";

beforeEach(() => {
  client.apiFetch.mockReset();
});

function setup() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client: queryClient }, children);
  return { queryClient, wrapper };
}

type IntervalFn = (query: { state: { data: PairingStatus | undefined } }) => number | false;

function intervalOf(queryClient: QueryClient, key: readonly unknown[]): IntervalFn {
  const query = queryClient.getQueryCache().find({ queryKey: key });
  return (query!.options as { refetchInterval?: unknown }).refetchInterval as IntervalFn;
}

const at = (data: PairingStatus | undefined) => ({ state: { data } });

describe("useChannels", () => {
  it("reads GET /channels and polls slowly", async () => {
    client.apiFetch.mockResolvedValue({ channels: [] });
    const { queryClient, wrapper } = setup();
    const { result } = renderHook(() => useChannels(), { wrapper });
    await waitFor(() => expect(result.current.data).toEqual({ channels: [] }));
    expect(client.apiFetch).toHaveBeenCalledWith("/channels");
    const query = queryClient.getQueryCache().find({ queryKey: keys.channels.all });
    expect((query!.options as { refetchInterval?: unknown }).refetchInterval).toBe(POLL.slow);
    expect(POLL.slow).toBe(30000);
  });
});

describe("useSetPrimaryChannel", () => {
  it("PUTs the channel, and null to clear, without retry", async () => {
    client.apiFetch.mockResolvedValue(undefined);
    const { queryClient, wrapper } = setup();
    const spy = vi.spyOn(queryClient, "invalidateQueries");
    const { result } = renderHook(() => useSetPrimaryChannel(), { wrapper });
    await act(async () => {
      await result.current.mutateAsync("telegram");
    });
    expect(client.apiFetch).toHaveBeenCalledWith("/channels/primary", {
      method: "PUT",
      body: JSON.stringify({ channel: "telegram" }),
    });
    await act(async () => {
      await result.current.mutateAsync(null);
    });
    expect(client.apiFetch).toHaveBeenLastCalledWith("/channels/primary", {
      method: "PUT",
      body: JSON.stringify({ channel: null }),
    });
    expect(queryClient.getMutationCache().getAll()[0]!.options.retry).toBe(false);
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.channels.all });
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.system.notifyPolicy });
  });
});

describe("useStartPairing", () => {
  it("POSTs without a body, keeps the code out of the cache and resets the status to waiting", async () => {
    const pairing = { code: "482913", command: "/pair 482913", expires_at: "2026-10-19T10:40:00Z", attempts_left: 5 };
    client.apiFetch.mockResolvedValue(pairing);
    const { queryClient, wrapper } = setup();
    queryClient.setQueryData(keys.channels.pairing("telegram"), { state: "expired", expires_at: "x" });
    const { result } = renderHook(() => useStartPairing("telegram"), { wrapper });
    let returned: unknown;
    await act(async () => {
      returned = await result.current.mutateAsync();
    });
    expect(client.apiFetch).toHaveBeenCalledWith("/channels/telegram/pairing", { method: "POST" });
    expect(returned).toEqual(pairing);
    expect(queryClient.getQueryData(keys.channels.pairing("telegram"))).toEqual({
      state: "waiting",
      expires_at: pairing.expires_at,
      attempts_left: 5,
    });
    expect(JSON.stringify(queryClient.getQueryCache().getAll().map((q) => q.state.data))).not.toContain("482913");
    expect(queryClient.getMutationCache().getAll()[0]!.options.retry).toBe(false);
  });

  it("encodes the channel id in the path", async () => {
    client.apiFetch.mockResolvedValue({ code: "1", command: "c", expires_at: "e", attempts_left: 5 });
    const { wrapper } = setup();
    const { result } = renderHook(() => useStartPairing("a/b"), { wrapper });
    await act(async () => {
      await result.current.mutateAsync();
    });
    expect(client.apiFetch).toHaveBeenCalledWith("/channels/a%2Fb/pairing", { method: "POST" });
  });
});

describe("usePairingStatus", () => {
  it("does not request while disabled", async () => {
    const { wrapper } = setup();
    renderHook(() => usePairingStatus("telegram", { enabled: false }), { wrapper });
    await new Promise((r) => setTimeout(r, 20));
    expect(client.apiFetch).not.toHaveBeenCalled();
  });

  it("polls fast while undefined or waiting, stops on terminal states", async () => {
    client.apiFetch.mockResolvedValue({ state: "waiting", expires_at: "e", attempts_left: 4 });
    const { queryClient, wrapper } = setup();
    const { result } = renderHook(() => usePairingStatus("telegram", { enabled: true }), { wrapper });
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(client.apiFetch).toHaveBeenCalledWith("/channels/telegram/pairing");
    const interval = intervalOf(queryClient, keys.channels.pairing("telegram"));
    expect(interval(at(undefined))).toBe(POLL.fast);
    expect(interval(at({ state: "waiting", expires_at: "e", attempts_left: 4 }))).toBe(POLL.fast);
    expect(interval(at({ state: "expired", expires_at: "e" }))).toBe(false);
    expect(interval(at({ state: "exhausted", attempts_left: 0 }))).toBe(false);
    expect(interval(at({ state: "superseded" }))).toBe(false);
    expect(
      interval(
        at({
          state: "paired",
          paired_at: "p",
          identity: {
            sender_ref: "1",
            sender_name: null,
            conversation_ref: "1",
            conversation_kind: "private",
            parent: null,
            home: true,
          },
        }),
      ),
    ).toBe(false);
  });

  it("keeps polling on none only while the local code has not expired", async () => {
    client.apiFetch.mockResolvedValue({ state: "none" });
    const future = new Date(Date.now() + 60_000).toISOString();
    const past = new Date(Date.now() - 60_000).toISOString();

    const live = setup();
    const a = renderHook(() => usePairingStatus("telegram", { enabled: true, expiresAt: future }), {
      wrapper: live.wrapper,
    });
    await waitFor(() => expect(a.result.current.data).toBeDefined());
    expect(intervalOf(live.queryClient, keys.channels.pairing("telegram"))(at({ state: "none" }))).toBe(POLL.fast);

    const gone = setup();
    const b = renderHook(() => usePairingStatus("telegram", { enabled: true, expiresAt: past }), {
      wrapper: gone.wrapper,
    });
    await waitFor(() => expect(b.result.current.data).toBeDefined());
    expect(intervalOf(gone.queryClient, keys.channels.pairing("telegram"))(at({ state: "none" }))).toBe(false);

    const bare = setup();
    const c = renderHook(() => usePairingStatus("telegram", { enabled: true }), { wrapper: bare.wrapper });
    await waitFor(() => expect(c.result.current.data).toBeDefined());
    expect(intervalOf(bare.queryClient, keys.channels.pairing("telegram"))(at({ state: "none" }))).toBe(false);
  });
});

describe("useCancelPairing", () => {
  it("DELETEs the pairing and invalidates it and the readout", async () => {
    client.apiFetch.mockResolvedValue(undefined);
    const { queryClient, wrapper } = setup();
    const spy = vi.spyOn(queryClient, "invalidateQueries");
    const { result } = renderHook(() => useCancelPairing("telegram"), { wrapper });
    await act(async () => {
      await result.current.mutateAsync();
    });
    expect(client.apiFetch).toHaveBeenCalledWith("/channels/telegram/pairing", { method: "DELETE" });
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.channels.pairing("telegram") });
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.channels.all });
  });
});

describe("useChannelIdentities", () => {
  it("reads the identities without polling", async () => {
    client.apiFetch.mockResolvedValue({ channel: "telegram", home: null, identities: [], projects: [] });
    const { queryClient, wrapper } = setup();
    const { result } = renderHook(() => useChannelIdentities("telegram"), { wrapper });
    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(client.apiFetch).toHaveBeenCalledWith("/channels/telegram/identities");
    const query = queryClient.getQueryCache().find({ queryKey: keys.channels.identities("telegram") });
    expect((query!.options as { refetchInterval?: unknown }).refetchInterval).toBeUndefined();
  });
});

describe("useRemoveIdentity", () => {
  it("removes a whole sender, encoding the ref", async () => {
    client.apiFetch.mockResolvedValue({ removed: 2, home_cleared: false });
    const { queryClient, wrapper } = setup();
    const spy = vi.spyOn(queryClient, "invalidateQueries");
    const { result } = renderHook(() => useRemoveIdentity("telegram"), { wrapper });
    await act(async () => {
      await result.current.mutateAsync({ sender: "a/4242" });
    });
    expect(client.apiFetch).toHaveBeenCalledWith("/channels/telegram/identities/a%2F4242", { method: "DELETE" });
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.channels.identities("telegram") });
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.channels.all });
    expect(queryClient.getMutationCache().getAll()[0]!.options.retry).toBe(false);
  });

  it("removes one conversation with the encoded query", async () => {
    client.apiFetch.mockResolvedValue({ removed: 1, home_cleared: true });
    const { wrapper } = setup();
    const { result } = renderHook(() => useRemoveIdentity("telegram"), { wrapper });
    await act(async () => {
      await result.current.mutateAsync({ sender: "4242", conversation: "-100123:5" });
    });
    expect(client.apiFetch).toHaveBeenCalledWith(
      "/channels/telegram/identities/4242?conversation=" + encodeURIComponent("-100123:5"),
      { method: "DELETE" },
    );
  });
});

describe("useSetHomeConversation", () => {
  it("PUTs the conversation as home and invalidates", async () => {
    client.apiFetch.mockResolvedValue({ home: "4242", previous: null });
    const { queryClient, wrapper } = setup();
    const spy = vi.spyOn(queryClient, "invalidateQueries");
    const { result } = renderHook(() => useSetHomeConversation("telegram"), { wrapper });
    await act(async () => {
      await result.current.mutateAsync({ sender: "4242", conversation_ref: "4242" });
    });
    expect(client.apiFetch).toHaveBeenCalledWith("/channels/telegram/identities/4242/home", {
      method: "PUT",
      body: JSON.stringify({ conversation_ref: "4242" }),
    });
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.channels.identities("telegram") });
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.channels.all });
  });
});
