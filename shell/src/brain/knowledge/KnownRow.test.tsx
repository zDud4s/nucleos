import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { daemonWith, known, renderRows } from "./test-helpers";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("KnownRow", () => {
  it("four kinds wear one tone, because a kind is not a state", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, kind: "prompt" }),
        known({ id: 2, kind: "memory" }),
        known({ id: 3, kind: "skill" }),
        known({ id: 4, kind: "subagent" }),
      ]),
    );

    await renderRows();

    for (const [kind, word] of [["prompt", "instruction"], ["memory", "fact"], ["skill", "how-to"], ["subagent", "delegation"]] as const) {
      const badge = await screen.findByText(word);
      expect(badge.className, kind).toContain("ui-badge-info");
    }
    expect(screen.queryByText("instruction")?.className).not.toContain("ui-badge-shadow");
  });

  it("does not ask for a chain until somebody opens one", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith(
        [known({ id: 4, status: "active", title: "current text", supersedes: 3 })],
        {
          replaced: [known({ id: 3, status: "superseded", title: "what it said before" })],
        },
      ),
    );

    await renderRows();
    await screen.findByText("current text");

    // One request per row would make reading a list of forty cost forty-one
    // calls to answer a question nobody has asked yet.
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/knowledge/4", expect.anything());
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/knowledge/4");

    fireEvent.click(screen.getByRole("button", { name: /What it replaced/ }));

    expect(await screen.findByText("what it said before")).toBeDefined();
  });

  it("shows the four layers, each row naming its layer and its source", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, layer: "semantic", status: "proposed", source: "owner", title: "a fact" }),
        known({ id: 2, layer: "episodic", status: "active", source: "consolidator", title: "a measurement" }),
        known({ id: 3, layer: "working", status: "live", source: "run", title: "job context" }),
        known({ id: 4, layer: "procedural", status: "rejected", source: "owner", title: "a method" }),
      ]),
    );

    await renderRows();

    for (const [title, layer, source] of [
      ["a fact", "semantic", "owner"],
      ["a measurement", "episodic", "consolidator"],
      ["job context", "working", "run"],
      ["a method", "procedural", "owner"],
    ] as const) {
      const row = (await screen.findByText(title)).closest(".learned-row");
      expect(row).not.toBeNull();
      expect(within(row as HTMLElement).getByText(layer)).toBeDefined();
      expect(within(row as HTMLElement).getByText(source)).toBeDefined();
    }
  });

  it("a measured row says how many times it was measured", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([known({ layer: "episodic", source: "consolidator", observations: 3 })]),
    );

    await renderRows();

    expect(await screen.findByText("measured 3 times")).toBeDefined();
  });

  it("evidence for a run is a link to that run and an unknown tag draws nothing", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({
          id: 1,
          evidence: JSON.stringify([
            { t: "run", id: 900449 },
            { t: "unknown", id: 2 },
          ]),
        }),
      ]),
    );

    await renderRows();
    const link = await screen.findByRole("link", { name: "run 900449" });

    expect(link.getAttribute("href")).toContain("/runs/900449");
    expect(screen.queryByText("unknown 2")).toBeNull();
  });

  it("knowledge evidence opens that row's history", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith(
        [
          known({ id: 1, evidence: JSON.stringify([{ t: "knowledge", id: 2 }]) }),
          known({ id: 2, title: "the evidence row" }),
        ],
        { replaced: [known({ id: 8, title: "older evidence" })] },
      ),
    );

    await renderRows();
    fireEvent.click(await screen.findByRole("button", { name: "knowledge 2" }));

    expect(await screen.findByText("older evidence")).toBeDefined();
    expect(daemon.apiFetch).toHaveBeenCalledWith("/knowledge/2");
  });

  it("a row flagged as a near-duplicate says of which, and opens that row", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith(
        [
          known({ id: 2, title: "the older learning" }),
          known({ id: 5, title: "the newer learning" }),
        ],
        undefined,
        [],
        [{ id: 5, of_id: 2 }],
      ),
    );

    await renderRows();

    expect(await screen.findByText("possible duplicate")).toBeDefined();
    expect(screen.getAllByText("possible duplicate")).toHaveLength(1);
    const line = screen.getByText("possible duplicate").closest("p") as HTMLElement;
    expect(line.textContent).toContain("the older learning");
    fireEvent.click(within(line).getByRole("button", { name: "#2" }));
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/knowledge/2"));
  });

  it("a distilled row says which job it came from, why, and links its runs", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith(
        [
          known({
            id: 5,
            source: "distiller",
            evidence: '[{"t":"job","id":7},{"t":"run","id":3}]',
          }),
        ],
        undefined,
        [{ id: 5, cause: "job_failed" }],
      ),
    );

    await renderRows();

    expect(await screen.findByText(/distilled from/)).toBeDefined();
    const jobLink = await screen.findAllByRole("link", { name: "job #7" });
    expect(jobLink[0].getAttribute("href")).toContain("/fleet");
    expect(await screen.findByText(/the job failed/)).toBeDefined();
    const runLink = await screen.findByRole("link", { name: "run 3" });
    expect(runLink.getAttribute("href")).toContain("/runs/3");
  });
});
