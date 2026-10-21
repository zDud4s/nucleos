import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn(), openStream: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));
// xyflow has no layout engine in happy-dom: a stand-in draws the nodes it is given (see
// fleet/xyflow-stand-in.ts). The real library is smoke-tested in fleet/FleetReal.test.tsx.
const flow = vi.hoisted(() => ({
  fitView: vi.fn(), setCenter: vi.fn(), zoom: 1, initialized: true, viewportInitialized: true,
  flowNodes: [] as Array<{ id: string; position: { x: number; y: number } }>,
  props: {} as Record<string, unknown>,
  renders: [] as Array<{ nodeTypes: unknown; edgeTypes: unknown }>,
}));
vi.mock("@xyflow/react", async (original) =>
  (await import("../fleet/xyflow-stand-in")).standIn(await original<typeof import("@xyflow/react")>(), flow),
);

import { Fleet } from "./Fleet";
import { ndjson, at, child, line, metricsOf, node, snapshot } from "../fleet/test-fixtures";
import { daemonFetch, daemonState, renderWithRouter } from "../test/harness";
import type { FleetEvent, FleetNode, TranscriptPage } from "../fleet/types";

const enc = new TextEncoder();
const ALL = { stop: true, open_run: true, full_transcript: true, pin: true };

/** A stream the test feeds by hand, like `LiveView.test.tsx`. */
function openPipe() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({ start: (c) => void (controller = c) });
  return {
    stream,
    send: (...events: FleetEvent[]) => controller.enqueue(enc.encode(ndjson(...events))),
    end: () => controller.close(),
  };
}

function respond(extra: (path: string, init?: RequestInit) => unknown = () => undefined) {
  const shared = daemonFetch(daemonState());
  return async (path: string, init?: RequestInit) => {
    if (path === "/concurrency") return { house: { limit: 8, held: 3 }, projects: [] };
    const answered = extra(path, init);
    if (answered !== undefined) return answered;
    return await shared(path, init);
  };
}

beforeEach(() => {
  for (const mock of Object.values(daemon)) mock.mockReset();
  daemon.apiFetch.mockImplementation(respond());
  daemon.apiText.mockResolvedValue("");
  flow.fitView.mockReset();
  flow.setCenter.mockReset();
  flow.viewportInitialized = true;
  flow.renders.length = 0;
});
afterEach(cleanup);

async function open(...first: FleetEvent[]) {
  const pipe = openPipe();
  daemon.openStream.mockResolvedValue(pipe.stream);
  const view = await renderWithRouter(<Fleet />);
  if (first.length > 0) await act(async () => pipe.send(...first));
  return { pipe, view };
}

describe("Fleet", () => {
  it("opens the live stream, and shows the empty state once an empty snapshot arrives", async () => {
    const { pipe } = await open();
    expect(daemon.openStream).toHaveBeenCalledWith("/fleet/live", expect.any(AbortSignal));
    expect(screen.queryByText("Nothing is running")).toBeNull(); // not before the first snapshot
    await act(async () => pipe.send(snapshot([])));
    expect(await screen.findByText("Nothing is running")).toBeDefined();
    expect(screen.getByText("● live")).toBeDefined();
  });

  it("draws every tree as nodes and shows the slots badge", async () => {
    const root = node("cc:r", { label: "nucleos-37", role: "orchestrate-tdd" });
    await open(snapshot([root, child("cc:k", root, { label: "chunk 2" }), node("cc:o", { label: "deal-sniper-c0", project: "deal_sniper" })]));
    expect(await screen.findByRole("article", { name: /^nucleos-37/ })).toBeDefined();
    expect(screen.getByRole("article", { name: /^chunk 2/ })).toBeDefined();
    expect(screen.getByRole("article", { name: /^deal-sniper-c0/ })).toBeDefined();
    expect(await screen.findByText("slots 3/8")).toBeDefined();
    expect(screen.queryByText("Nothing is running")).toBeNull();
  });

  it("a lines event reaches the terminal of its node only", async () => {
    const { pipe } = await open(snapshot([node("a", { label: "alpha" }), node("b", { label: "beta" })]));
    await screen.findByRole("article", { name: /^alpha/ });
    await act(async () => pipe.send({ t: "lines", node_id: "a", lines: [line("tool", "Read x.rs")] }));
    expect(within(screen.getByRole("article", { name: /^alpha/ })).getByText("Read x.rs")).toBeDefined();
    expect(within(screen.getByRole("article", { name: /^beta/ })).queryByText("Read x.rs")).toBeNull();
  });

  it("filters by project and by Active only", async () => {
    await open(
      snapshot([
        node("a", { label: "alpha", project: "nucleos" }),
        node("b", { label: "beta", project: "other" }),
        node("c", { label: "gamma", project: "other", state: "done", started_at: at(2) }),
      ]),
    );
    await screen.findByRole("article", { name: /^alpha/ });
    fireEvent.change(screen.getByLabelText("Project"), { target: { value: "other" } });
    expect(screen.queryByRole("article", { name: /^alpha/ })).toBeNull();
    expect(screen.getByRole("article", { name: /^gamma/ })).toBeDefined();
    fireEvent.click(screen.getByLabelText("Active only"));
    expect(screen.queryByRole("article", { name: /^gamma/ })).toBeNull();
    expect(screen.getByRole("article", { name: /^beta/ })).toBeDefined();
  });

  it("folds more than six done children into a group that expands", async () => {
    const root = node("r", { label: "root" });
    const done = Array.from({ length: 8 }, (_, i) =>
      child(`d${i}`, root, { label: `job ${i}`, state: "done", started_at: at(i + 1), ended_at: at(i + 2) }),
    );
    await open(snapshot([root, ...done]));
    expect(await screen.findByText("✓ 8 finished")).toBeDefined();
    expect(screen.queryByRole("article", { name: /^job 0/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Expand the finished nodes under root" }));
    expect(screen.getByRole("article", { name: /^job 0/ })).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Collapse the finished nodes under root" }));
    expect(screen.queryByRole("article", { name: /^job 0/ })).toBeNull();
  });

  it("selecting a node lights its path to the root and nothing else", async () => {
    const root = node("r", { label: "root" });
    const mid = child("m", root, { label: "mid" });
    const leaf = child("l", mid, { label: "leaf" });
    const sibling = child("s", root, { label: "sibling", started_at: at(5) });
    await open(snapshot([root, mid, leaf, sibling]));
    fireEvent.click(await screen.findByRole("article", { name: /^leaf/ }));
    const lit = (name: RegExp) => screen.getByRole("article", { name }).className.includes("fleet-node-path");
    expect([lit(/^leaf/), lit(/^mid/), lit(/^root/), lit(/^sibling/)]).toEqual([true, true, true, false]);
    fireEvent.click(screen.getByTestId("flow")); // the pane
    expect(lit(/^leaf/)).toBe(false);
  });

  it("opens the transcript panel from ⤢, pages it, and closes it with Escape", async () => {
    daemon.apiFetch.mockImplementation(
      respond((path) =>
        path === "/fleet/nodes/cc%3Ar/transcript"
          ? ({ entries: [{ kind: "text", text: "from the page", detail: null }], before: null } satisfies TranscriptPage)
          : undefined,
      ),
    );
    await open(snapshot([node("cc:r", { label: "nucleos-37", capabilities: ALL, run_id: 7 })]));
    fireEvent.click(await screen.findByRole("button", { name: "Open the full transcript" }));
    expect(await screen.findByText("from the page")).toBeDefined();
    expect(screen.getByRole("complementary", { name: "Transcript of nucleos-37" })).toBeDefined();
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("complementary")).toBeNull());
  });

  it("pins a root through the pin route and reflects the stream's pinned flag", async () => {
    const root = node("cc:r", { label: "nucleos-37", capabilities: ALL, run_id: 7 });
    const kid = child("cc:k", root, { label: "kid", capabilities: { ...ALL, pin: false } });
    const { pipe } = await open(snapshot([root, kid]));
    await screen.findByRole("article", { name: /^nucleos-37/ });
    expect(within(screen.getByRole("article", { name: /^kid/ })).queryByRole("button", { name: "Pin this tree" })).toBeNull();
    fireEvent.click(within(screen.getByRole("article", { name: /^nucleos-37/ })).getByRole("button", { name: "Pin this tree" }));
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/fleet/pins/cc%3Ar", { method: "POST" }));
    await act(async () => pipe.send({ t: "node", node: { ...root, pinned: true } as FleetNode }));
    expect(within(screen.getByRole("article", { name: /^nucleos-37/ })).getByRole("button", { name: "Pin this tree" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("says reconnecting when the stream drops, without losing the tree", async () => {
    const { pipe } = await open(snapshot([node("a", { label: "alpha" })], {}, [metricsOf("a")]));
    await screen.findByRole("article", { name: /^alpha/ });
    await act(async () => pipe.end());
    expect(await screen.findByText("◌ reconnecting…")).toBeDefined();
    expect(screen.getByRole("article", { name: /^alpha/ })).toBeDefined();
  });

  it("after three failed reconnects with nothing to show, says the stream cannot be reached", async () => {
    daemon.openStream.mockRejectedValue(new Error("down"));
    await renderWithRouter(<Fleet />);
    expect(screen.queryByText("Can't reach the fleet stream — retrying")).toBeNull();
    // Attempts fail at 0 s, 1 s and 3 s of the real backoff (1, 2 s): the third is the note.
    expect(await screen.findByText("Can't reach the fleet stream — retrying", {}, { timeout: 6000 })).toBeDefined();
    expect(screen.getByText("◌ reconnecting…")).toBeDefined();
  }, 10_000);

  it("announces a node waiting for you, politely", async () => {
    const a = node("a", { label: "alpha" });
    const { pipe } = await open(snapshot([a]));
    await screen.findByRole("article", { name: /^alpha/ });
    await act(async () => pipe.send({ t: "node", node: { ...a, state: "idle" } }));
    const region = document.querySelector("[aria-live='polite']")!;
    expect(region.textContent).toBe("alpha is waiting for you");
  });

  it("asks for nothing it cannot be given: no node is acted on by harness name", async () => {
    await open(snapshot([node("x:1", { label: "stranger", harness: "brand-new-harness", capabilities: { ...ALL, stop: false } })]));
    const card = await screen.findByRole("article", { name: /^stranger/ });
    expect(within(card).queryByRole("button", { name: /^Stop / })).toBeNull();
    expect(within(card).getByRole("button", { name: "Open the full transcript" })).toBeDefined();
  });
});
