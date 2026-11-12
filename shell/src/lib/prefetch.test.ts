import { QueryClient } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { bindPrefetchClient, prefetchHandlers, prefetchRoute, registerPrefetcher, registerRouteLoader, resetPrefetch } from "./prefetch";

describe("prefetch registry", () => {
  beforeEach(resetPrefetch);

  it("loads a route chunk once however often it is hovered", () => {
    const load = vi.fn(() => Promise.resolve());
    registerRouteLoader("/fleet", load);
    prefetchRoute("/fleet");
    prefetchRoute("/fleet");
    expect(load).toHaveBeenCalledTimes(1);
  });
  it("runs data prefetchers with the bound client, and unregisters", () => {
    const qc = new QueryClient();
    bindPrefetchClient(qc);
    const fn = vi.fn();
    const off = registerPrefetcher("/brain", fn);
    prefetchHandlers("/brain").onPointerEnter();
    expect(fn).toHaveBeenCalledWith(qc);
    off();
    prefetchRoute("/brain");
    expect(fn).toHaveBeenCalledTimes(1);
  });
  it("first hover runs a prefetcher that the loader registers, exactly once", async () => {
    const qc = new QueryClient();
    bindPrefetchClient(qc);
    const fn = vi.fn();
    const load = vi.fn(async () => {
      registerPrefetcher("/lazy", fn); // like a page module does on import
    });
    registerRouteLoader("/lazy", load);
    prefetchRoute("/lazy");
    prefetchRoute("/lazy"); // while the loader is still pending
    await vi.waitFor(() => expect(fn).toHaveBeenCalledTimes(1));
    expect(fn).toHaveBeenCalledWith(qc);
    expect(load).toHaveBeenCalledTimes(1);
  });
  it("swallows prefetcher failures", () => {
    bindPrefetchClient(new QueryClient());
    registerPrefetcher("/x", () => {
      throw new Error("boom");
    });
    expect(() => prefetchRoute("/x")).not.toThrow();
  });
});
