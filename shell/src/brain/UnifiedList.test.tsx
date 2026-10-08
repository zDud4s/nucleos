import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { OwnerNote } from "../data/owner-notes";
import type { CaptureRequest } from "../data/captures";
import type { Known } from "../data/knowledge";
import { renderWithRouter } from "../test/harness";
import { daemonWith, known } from "./knowledge/test-helpers";
import { UnifiedList } from "./UnifiedList";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function note(over: Partial<OwnerNote> = {}): OwnerNote {
  return {
    id: 1,
    text: "Rust owns the state\nsecond line",
    origin: "shell",
    state: "active",
    created_at: "2026-09-01T09:00:00+00:00",
    updated_at: "2026-09-01T09:00:00+00:00",
    ...over,
  };
}

function capture(over: Partial<CaptureRequest> = {}): CaptureRequest {
  return {
    job_id: 7,
    project_id: "nucleos",
    causes: [],
    prompt_text: "Why did the gate fail?\nmore",
    state: "answered",
    deadline: "2026-09-02T09:00:00+00:00",
    seconds_left: 0,
    note_id: null,
    created_at: "2026-09-01T10:00:00+00:00",
    closed_at: "2026-09-01T11:00:00+00:00",
    ...over,
  };
}

function daemonHolding(notes: OwnerNote[], rows: Known[], captures: CaptureRequest[] = []) {
  const knowledge = daemonWith(rows);
  daemon.apiFetch.mockImplementation((path: string) => {
    if (path === "/capture-requests?state=all") return Promise.resolve(captures);
    if (path === "/capture-requests") return Promise.resolve(captures.filter((c) => c.state === "open"));
    if (path.startsWith("/owner-notes")) return Promise.resolve(notes);
    return knowledge(path);
  });
}

describe("UnifiedList", () => {
  it("renders notes and knowledge, and a click reports the item address", async () => {
    daemonHolding([note({ id: 1 })], [known({ id: 5, title: "Suite needs PATH" })]);
    const onSelect = vi.fn();
    await renderWithRouter(<UnifiedList onSelect={onSelect} />);

    fireEvent.click(await screen.findByRole("button", { name: /Suite needs PATH/ }));
    expect(onSelect).toHaveBeenCalledWith("knowledge:5");
    fireEvent.click(screen.getByRole("button", { name: /Rust owns the state/ }));
    expect(onSelect).toHaveBeenCalledWith("note:1");
  });

  it("keeps proposed rows out of the default list, and decides them quickly under Any state", async () => {
    daemonHolding([], [
      known({ id: 4, title: "In force one" }),
      known({ id: 9, status: "proposed", proposal_id: 3, title: "Proposed thing" }),
    ]);
    await renderWithRouter(<UnifiedList onSelect={vi.fn()} />);

    // The default state keeps proposed rows out of the list itself.
    await screen.findByRole("button", { name: /In force one/ });
    expect(screen.queryByRole("button", { name: "Approve Proposed thing" })).toBeNull();

    const state = screen.getByRole("group", { name: "State" });
    fireEvent.click(within(state).getByRole("button", { name: "Any state" }));
    const approvals = await screen.findAllByRole("button", { name: "Approve Proposed thing" });
    // The quick decision carries the title; the full one is in the aside's "Lessons to approve".
    expect(approvals).toHaveLength(1);
    fireEvent.click(approvals[0] as HTMLElement);
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/3/approve", { method: "POST" }),
    );
  });

  it("the Type filter hides notes", async () => {
    daemonHolding([note({ id: 1 })], [known({ id: 5, title: "Suite needs PATH" })]);
    await renderWithRouter(<UnifiedList onSelect={vi.fn()} />);
    await screen.findByRole("button", { name: /Suite needs PATH/ });

    const type = screen.getByRole("group", { name: "Type" });
    fireEvent.click(within(type).getByRole("button", { name: "Knowledge" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: /Rust owns the state/ })).toBeNull());
    expect(screen.getByRole("button", { name: /Suite needs PATH/ })).toBeTruthy();
  });

  it("lists closed captures under Over, titled by the first line of the question", async () => {
    daemonHolding([], [known({ id: 5, title: "Suite needs PATH" })], [capture({ job_id: 7 })]);
    const onSelect = vi.fn();
    await renderWithRouter(<UnifiedList onSelect={onSelect} />);
    await screen.findByRole("button", { name: /Suite needs PATH/ });
    expect(screen.queryByRole("button", { name: /Why did the gate fail/ })).toBeNull();

    const state = screen.getByRole("group", { name: "State" });
    fireEvent.click(within(state).getByRole("button", { name: "Over" }));
    fireEvent.click(await screen.findByRole("button", { name: /Why did the gate fail\?/ }));
    expect(onSelect).toHaveBeenCalledWith("capture:7");
  });

  it("shows the Teach callout when there is nothing at all", async () => {
    daemonHolding([], []);
    await renderWithRouter(<UnifiedList onSelect={vi.fn()} />);
    expect(await screen.findByText("Nothing has been learned yet")).toBeTruthy();
  });
});
