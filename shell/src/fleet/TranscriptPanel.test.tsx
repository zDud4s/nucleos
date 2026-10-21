import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import { FleetStoreContext } from "./hooks";
import { FleetStore } from "./store";
import { line, node, snapshot } from "./test-fixtures";
import { TranscriptPanel } from "./TranscriptPanel";
import type { TranscriptPage } from "./types";

const ID = "cc:a";
const URL0 = "/fleet/nodes/cc%3Aa/transcript";

function page(texts: string[], before: string | null, detail: Record<string, string> = {}): TranscriptPage {
  return {
    before,
    entries: texts.map((text) => ({ kind: text.startsWith("Read") ? "tool" : "text", text, detail: detail[text] ?? null })),
  };
}

function mount(onClose = vi.fn()) {
  const store = new FleetStore();
  store.apply(snapshot([node(ID, { label: "nucleos-37", role: "chat" })]));
  const view = render(
    <FleetStoreContext.Provider value={store}>
      <TranscriptPanel key={ID} id={ID} onClose={onClose} />
    </FleetStoreContext.Provider>,
  );
  return { store, onClose, view };
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("TranscriptPanel", () => {
  it("reads the latest page and shows it oldest first", async () => {
    daemon.apiFetch.mockResolvedValue(page(["first", "Read x", "last"], null));
    mount();
    expect(await screen.findByText("last")).toBeDefined();
    expect(daemon.apiFetch).toHaveBeenCalledWith(URL0);
    const rows = screen.getAllByText(/^(first|Read x|last)$/).map((r) => r.textContent);
    expect(rows).toEqual(["first", "Read x", "last"]);
    expect(screen.getByRole("complementary", { name: "Transcript of nucleos-37" })).toBeDefined();
    expect(screen.queryByRole("button", { name: "Load earlier" })).toBeNull(); // nothing earlier
  });

  it("pages backwards with Load earlier and puts the older entries on top", async () => {
    daemon.apiFetch
      .mockResolvedValueOnce(page(["newer 1", "newer 2"], "c1"))
      .mockResolvedValueOnce(page(["older 1", "older 2"], null));
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Load earlier" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenLastCalledWith(`${URL0}?before=c1`),
    );
    expect(await screen.findByText("older 1")).toBeDefined();
    const rows = screen.getAllByText(/^(older|newer) /).map((r) => r.textContent);
    expect(rows).toEqual(["older 1", "older 2", "newer 1", "newer 2"]);
    expect(screen.queryByRole("button", { name: "Load earlier" })).toBeNull();
  });

  it("expands a tool to its full input and result on click, as plain text", async () => {
    const hostile = "<img src=x onerror=alert(1)>\nfull output";
    daemon.apiFetch.mockResolvedValue(page(["Read big.rs"], null, { "Read big.rs": hostile }));
    const { view } = mount();
    const tool = await screen.findByRole("button", { name: "Read big.rs" });
    expect(tool.getAttribute("aria-expanded")).toBe("false");
    expect(view.container.querySelector("pre")).toBeNull();
    fireEvent.click(tool);
    expect(tool.getAttribute("aria-expanded")).toBe("true");
    expect(view.container.querySelector("pre")?.textContent).toBe(hostile);
    expect(view.container.querySelector("img")).toBeNull();
  });

  it("appends new lines from the stream after the page", async () => {
    daemon.apiFetch.mockResolvedValue(page(["one"], null));
    const { store } = mount();
    await screen.findByText("one");
    act(() => store.apply({ t: "lines", node_id: ID, lines: [line("text", "arrived")] }));
    expect(screen.getByText("arrived")).toBeDefined();
  });

  it("keeps lines that arrive while the page is loading, minus the ones the page contains", async () => {
    let resolve!: (p: TranscriptPage) => void;
    daemon.apiFetch.mockReturnValue(new Promise<TranscriptPage>((r) => (resolve = r)));
    const { store } = mount();
    expect(screen.getByRole("status").textContent).toContain("Loading");
    act(() => {
      store.apply({ t: "lines", node_id: ID, lines: [line("text", "in the page"), line("text", "only live")] });
    });
    await act(async () => resolve(page(["older", "in the page"], null)));
    expect(screen.getAllByText("in the page")).toHaveLength(1);
    expect(screen.getAllByText("only live")).toHaveLength(1);
  });

  it("keeps de-duplicating the stream for a short grace window after the page arrived, then stops", async () => {
    let clock = 10_000;
    vi.spyOn(Date, "now").mockImplementation(() => clock);
    daemon.apiFetch.mockResolvedValue(page(["one", "two"], null));
    const { store } = mount();
    await screen.findByText("two");
    // The same line comes in again just after the page was read: it is the page's own tail.
    act(() => store.apply({ t: "lines", node_id: ID, lines: [line("text", "two")] }));
    expect(screen.getAllByText("two")).toHaveLength(1);
    // Later the same text is a genuine repeat and is shown.
    clock += 3_000;
    act(() => store.apply({ t: "lines", node_id: ID, lines: [line("text", "two")] }));
    expect(screen.getAllByText("two")).toHaveLength(2);
  });

  it("still shows what arrived while the page was loading when the page cannot be read", async () => {
    let reject!: (error: unknown) => void;
    daemon.apiFetch.mockReturnValue(new Promise<TranscriptPage>((_, r) => (reject = r)));
    const { store } = mount();
    act(() => store.apply({ t: "lines", node_id: ID, lines: [line("text", "said while loading")] }));
    await act(async () => reject(new Error("down")));
    expect(await screen.findByText("The transcript could not be read.")).toBeDefined();
    expect(screen.getAllByText("said while loading")).toHaveLength(1);
  });

  it("says why there is nothing to show", async () => {
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(409, "no_transcript", "no_transcript"));
    mount();
    expect(await screen.findByText("No full transcript is kept for this node.")).toBeDefined();
    cleanup();
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(404, "unknown_node", "unknown_node"));
    mount();
    expect(await screen.findByText("This node is gone.")).toBeDefined();
    cleanup();
    daemon.apiFetch.mockRejectedValue(new Error("down"));
    mount();
    expect(await screen.findByText("The transcript could not be read.")).toBeDefined();
  });

  it("Escape and the close button close it, and focus starts on the close button", async () => {
    daemon.apiFetch.mockResolvedValue(page(["one"], null));
    const { onClose } = mount();
    await screen.findByText("one");
    const close = screen.getByRole("button", { name: "Close the transcript" });
    expect(document.activeElement).toBe(close);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.click(close);
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("says who it is: label, role and state", async () => {
    daemon.apiFetch.mockResolvedValue(page(["one"], null));
    mount();
    const panel = await screen.findByRole("complementary");
    expect(within(panel).getByText("chat")).toBeDefined();
    expect(within(panel).getByText(/running/)).toBeDefined();
  });
});
