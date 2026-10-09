import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { ContextRefs, droppedRoot } from "./ContextRefs";
import { renderWithQuery } from "../test/harness";
import { ApiRefusal } from "../data/client";
import type { ContextRef } from "../data/context-refs";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { listen } from "@tauri-apps/api/event";

type Drop = (event: { payload: unknown }) => void;
let dropped: Drop | undefined;

function ref(over: Partial<ContextRef> = {}): ContextRef {
  return {
    id: 1,
    owner_kind: "agent",
    owner_id: "copywriter",
    path: "C:/files/brief.md",
    kind: "file",
    note: "the brief",
    created_at: "2026-10-09T09:00:00Z",
    state: "file",
    ...over,
  };
}

/** A daemon that holds `rows` for the owner and answers the four routes from them. */
function holding(rows: ContextRef[], refuse?: (method: string) => Error | undefined) {
  let nextId = 100;
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    const refused = refuse?.(method);
    if (refused !== undefined) throw refused;
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
    const one = /^\/context-refs\/(agent|team)\/[^/]+\/(\d+)$/.exec(path);
    if (one !== null && method === "PUT") {
      const row = rows.find((r) => r.id === Number(one[2]));
      if (row !== undefined) row.note = body.note;
      return row;
    }
    if (one !== null && method === "DELETE") {
      rows.splice(
        rows.findIndex((r) => r.id === Number(one[2])),
        1,
      );
      return undefined;
    }
    if (/^\/context-refs\/(agent|team)\/[^/]+$/.test(path)) {
      if (method === "POST") {
        const created = ref({ id: nextId++, path: body.path, note: body.note ?? null });
        rows.push(created);
        return created;
      }
      return rows.map((r) => ({ ...r }));
    }
    throw new Error(`unexpected ${method} ${path}`);
  });
}

/** The dwell `ConfirmButton` needs between arming and confirming. */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

function calls(method: string): [string, RequestInit][] {
  return daemon.apiFetch.mock.calls.filter(([, init]) => (init?.method ?? "GET") === method);
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  dropped = undefined;
  vi.mocked(listen).mockReset();
  vi.mocked(listen).mockImplementation(async (name: string, handler: unknown) => {
    if (name === "files://dropped") dropped = handler as Drop;
    return () => {};
  });
});

describe("ContextRefs - the owner's context files", () => {
  it("lists each ref with its path and note and asks the núcleo for this owner only", async () => {
    holding([ref(), ref({ id: 2, path: "C:/files/docs", kind: "dir", note: null, state: "dir" })]);

    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);

    const region = await screen.findByRole("region", { name: "Context" });
    expect(await within(region).findByText("C:/files/brief.md")).toBeDefined();
    expect(within(region).getByText("the brief")).toBeDefined();
    expect(within(region).getByText("C:/files/docs")).toBeDefined();
    expect(calls("GET").map(([path]) => path)).toEqual(["/context-refs/agent/copywriter"]);
  });

  it("shows the missing indicator only on a ref the núcleo says is missing", async () => {
    holding([ref(), ref({ id: 2, path: "C:/files/gone.md", state: "missing", note: null })]);

    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);

    const gone = (await screen.findByText("C:/files/gone.md")).closest("li") as HTMLElement;
    expect(within(gone).getByText("missing")).toBeDefined();
    const present = screen.getByText("C:/files/brief.md").closest("li") as HTMLElement;
    expect(within(present).queryByText("missing")).toBeNull();
  });

  it("says so when the owner carries nothing", async () => {
    holding([]);

    renderWithQuery(<ContextRefs ownerKind="team" ownerId="financas" />);

    expect(await screen.findByText("No context files yet.")).toBeDefined();
    expect(calls("GET").map(([path]) => path)).toEqual(["/context-refs/team/financas"]);
  });

  it("says the núcleo did not answer rather than showing an empty list", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("down"));

    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);

    expect(await screen.findByText(/the núcleo did not answer/)).toBeDefined();
    expect(screen.queryByText("No context files yet.")).toBeNull();
  });

  it("adds a typed path with its note and lists the new ref", async () => {
    holding([]);
    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);
    await screen.findByText("No context files yet.");

    fireEvent.change(screen.getByLabelText("Path"), { target: { value: "C:/files/new.md" } });
    fireEvent.change(screen.getByLabelText("Note"), { target: { value: "read first" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(await screen.findByText("C:/files/new.md")).toBeDefined();
    expect(calls("POST")).toHaveLength(1);
    expect(calls("POST")[0][0]).toBe("/context-refs/agent/copywriter");
    expect(JSON.parse(String(calls("POST")[0][1].body))).toEqual({
      path: "C:/files/new.md",
      note: "read first",
    });
    expect((screen.getByLabelText("Path") as HTMLInputElement).value).toBe("");
  });

  it("will not add an empty path", async () => {
    holding([]);
    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);
    await screen.findByText("No context files yet.");

    expect((screen.getByRole("button", { name: "Add" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("shows the núcleo's own sentence when it refuses a path", async () => {
    holding([], (method) =>
      method === "POST"
        ? new ApiRefusal(422, "unprocessable", "the path is outside the folders this owner may use")
        : undefined,
    );
    renderWithQuery(<ContextRefs ownerKind="team" ownerId="financas" />);
    await screen.findByText("No context files yet.");

    fireEvent.change(screen.getByLabelText("Path"), { target: { value: "C:/elsewhere/x" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(
      await screen.findByText("the path is outside the folders this owner may use"),
    ).toBeDefined();
  });

  it("edits a note in place and sends only the note", async () => {
    holding([ref()]);
    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);
    await screen.findByText("the brief");

    fireEvent.click(screen.getByRole("button", { name: "Edit note" }));
    fireEvent.change(screen.getByLabelText("Note for C:/files/brief.md"), {
      target: { value: "the new brief" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save note" }));

    expect(await screen.findByText("the new brief")).toBeDefined();
    const put = calls("PUT");
    expect(put).toHaveLength(1);
    expect(put[0][0]).toBe("/context-refs/agent/copywriter/1");
    expect(JSON.parse(String(put[0][1].body))).toEqual({ note: "the new brief" });
  });

  it("cancelling a note edit sends nothing", async () => {
    holding([ref()]);
    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);
    await screen.findByText("the brief");

    fireEvent.click(screen.getByRole("button", { name: "Edit note" }));
    fireEvent.change(screen.getByLabelText("Note for C:/files/brief.md"), {
      target: { value: "never sent" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(calls("PUT")).toHaveLength(0);
    expect(screen.getByText("the brief")).toBeDefined();
  });

  it("removes a ref only on the second press", async () => {
    holding([ref()]);
    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);
    await screen.findByText("C:/files/brief.md");

    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    expect(calls("DELETE")).toHaveLength(0);
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: /Remove C:\/files\/brief\.md/ }));

    await waitFor(() => expect(calls("DELETE")).toHaveLength(1));
    expect(calls("DELETE")[0][0]).toBe("/context-refs/agent/copywriter/1");
    expect(await screen.findByText("No context files yet.")).toBeDefined();
  });

  it("adds what is dropped on the window, a folder as the folder itself and once", async () => {
    holding([]);
    renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);
    await screen.findByText("No context files yet.");
    await waitFor(() => expect(dropped).toBeDefined());

    await act(async () => {
      dropped?.({
        payload: {
          truncated: false,
          files: [
            { path: "C:\\work\\brief.md", folder: "", name: "brief.md", size: 4 },
            { path: "C:\\work\\Docs\\a.md", folder: "Docs", name: "a.md", size: 1 },
            { path: "C:\\work\\Docs\\sub\\b.md", folder: "Docs/sub", name: "b.md", size: 1 },
          ],
        },
      });
    });

    await waitFor(() => expect(calls("POST")).toHaveLength(2));
    const sent = calls("POST").map(([, init]) => JSON.parse(String(init.body)).path);
    expect(sent).toEqual(["C:\\work\\brief.md", "C:\\work\\Docs"]);
  });

  it("stops listening for drops when it goes away", async () => {
    const unlisten = vi.fn();
    vi.mocked(listen).mockImplementation(async () => unlisten);
    holding([]);
    const view = renderWithQuery(<ContextRefs ownerKind="agent" ownerId="copywriter" />);
    await screen.findByText("No context files yet.");

    view.unmount();

    await waitFor(() => expect(unlisten).toHaveBeenCalled());
  });
});

describe("droppedRoot - the folder a dropped file was resolved from", () => {
  it("is the file itself when it was dropped on its own", () => {
    expect(droppedRoot({ path: "C:\\w\\a.md", folder: "", name: "a.md", size: 1 })).toBe(
      "C:\\w\\a.md",
    );
  });

  it("climbs back to the dropped folder through its sub-folders", () => {
    expect(
      droppedRoot({ path: "/w/Docs/sub/b.md", folder: "Docs/sub", name: "b.md", size: 1 }),
    ).toBe("/w/Docs");
  });
});
