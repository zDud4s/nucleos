import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { ApiRefusal } from "../../data/client";
import { daemonWith, known, panelFor, renderWaiting } from "./test-helpers";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("WaitingPanel", () => {
  it("the waiting queue is grouped by scope and source with a count per group", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 4, status: "proposed", scope_id: "beta", source: "run", title: "beta run" }),
        known({ id: 1, status: "proposed", scope_id: "alpha", source: "owner", title: "alpha owner one" }),
        known({ id: 3, status: "proposed", scope_id: "alpha", source: "run", title: "alpha run" }),
        known({ id: 2, status: "proposed", scope_id: "alpha", source: "owner", title: "alpha owner two" }),
      ]),
    );

    await renderWaiting();
    const panel = await panelFor("Lessons to approve");
    const groups = panel.querySelectorAll(".learned-group");

    expect(groups).toHaveLength(3);
    expect(groups[0].textContent).toContain("alpha");
    expect(groups[0].textContent).toContain("owner");
    expect(groups[0].textContent).toContain("2");
    expect(groups[0].textContent).toContain("alpha owner one");
    expect(groups[0].textContent).toContain("alpha owner two");
    expect(groups[1].textContent).toContain("alpha run");
    expect(groups[2].textContent).toContain("beta run");
  });

  it("approve all sends one approve per proposal in ascending order, one at a time", async () => {
    const rows = [
      known({ id: 3, status: "proposed", proposal_id: 30, title: "third" }),
      known({ id: 1, status: "proposed", proposal_id: 10, title: "first" }),
      known({ id: 2, status: "proposed", proposal_id: 20, title: "second" }),
    ];
    const read = daemonWith(rows);
    const releases: Array<() => void> = [];
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => {
      if (init?.method === "POST") {
        return new Promise((resolve) => releases.push(() => resolve({ refinement_id: 1 })));
      }
      return read(path);
    });

    await renderWaiting();
    fireEvent.click(await screen.findByRole("button", { name: "Approve all 3" }));
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(screen.getByRole("button", { name: "Let all 3 into every later prompt" }));

    await waitFor(() => {
      const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
      expect(posts.map(([path]) => path)).toEqual(["/proposals/10/approve"]);
    });
    releases[0]();
    await waitFor(() => {
      const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
      expect(posts.map(([path]) => path)).toEqual([
        "/proposals/10/approve",
        "/proposals/20/approve",
      ]);
    });
    releases[1]();
    await waitFor(() => {
      const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
      expect(posts.map(([path]) => path)).toEqual([
        "/proposals/10/approve",
        "/proposals/20/approve",
        "/proposals/30/approve",
      ]);
    });
    releases[2]();
    await waitFor(() =>
      expect(screen.getAllByRole("button", { name: "Approve" })[0].hasAttribute("disabled")).toBe(
        false,
      ),
    );
  });

  it("approve all asks for a second click first", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, status: "proposed", proposal_id: 10 }),
        known({ id: 2, status: "proposed", proposal_id: 20 }),
      ]),
    );

    await renderWaiting();
    fireEvent.click(await screen.findByRole("button", { name: "Approve all 2" }));

    expect(daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST")).toEqual([]);
    expect(
      screen.getByRole("button", { name: "Let all 2 into every later prompt" }),
    ).toBeDefined();
  });

  it("refuse all sends one reject per proposal", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 2, status: "proposed", proposal_id: 22 }),
        known({ id: 1, status: "proposed", proposal_id: 11 }),
      ]),
    );

    await renderWaiting();
    fireEvent.click(await screen.findByRole("button", { name: "Refuse all 2" }));

    await waitFor(() => {
      const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
      expect(posts.map(([path]) => path)).toEqual([
        "/proposals/11/reject",
        "/proposals/22/reject",
      ]);
    });
  });

  it("a batch that partly fails says how many were decided and keeps going", async () => {
    const rows = [
      known({ id: 1, status: "proposed", proposal_id: 11 }),
      known({ id: 2, status: "proposed", proposal_id: 22 }),
      known({ id: 3, status: "proposed", proposal_id: 33 }),
    ];
    const read = daemonWith(rows);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method !== "POST") return read(path);
      if (path === "/proposals/22/reject") throw new ApiRefusal(409, "conflict", "");
      return undefined;
    });

    await renderWaiting();
    fireEvent.click(await screen.findByRole("button", { name: "Refuse all 3" }));

    expect(await screen.findByText(/2 decided; 1 could not be/)).toBeDefined();
    const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
    expect(posts.map(([path]) => path)).toEqual([
      "/proposals/11/reject",
      "/proposals/22/reject",
      "/proposals/33/reject",
    ]);
  });

  it("a group of one has no batch buttons", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([known({ id: 1, status: "proposed", proposal_id: 11 })]),
    );

    await renderWaiting();
    await screen.findByRole("button", { name: "Approve" });

    expect(screen.queryByRole("button", { name: /Approve all/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Refuse all/ })).toBeNull();
  });
});

describe("WaitingPanel - scope chooser", () => {
  const posts = () => daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");

  it("scope chooser sends the row's declared scope by default", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({
          id: 1,
          status: "proposed",
          proposal_id: 11,
          scope_kind: "project",
          scope_id: "nucleos",
        }),
      ]),
    );

    await renderWaiting();
    const kind = await screen.findByRole("combobox", { name: "Approve into" });
    expect((kind as HTMLSelectElement).value).toBe("project");
    expect((screen.getByRole("textbox", { name: "Scope id" }) as HTMLInputElement).value).toBe(
      "nucleos",
    );
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));

    await waitFor(() => expect(posts()).toHaveLength(1));
    const [path, init] = posts()[0];
    expect(path).toBe("/proposals/11/approve");
    expect(JSON.parse(init.body as string).scope).toBe("project:nucleos");
  });

  it("scope chooser sends the scope the owner chose", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([known({ id: 1, status: "proposed", proposal_id: 11 })]),
    );

    await renderWaiting();
    fireEvent.change(await screen.findByRole("combobox", { name: "Approve into" }), {
      target: { value: "agent" },
    });
    fireEvent.change(screen.getByRole("textbox", { name: "Scope id" }), {
      target: { value: "copywriter" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));

    await waitFor(() => expect(posts()).toHaveLength(1));
    const [path, init] = posts()[0];
    expect(path).toBe("/proposals/11/approve");
    expect(JSON.parse(init.body as string).scope).toBe("agent:copywriter");
  });

  it("scope chooser approves a machine row with no scope until one is chosen", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({
          id: 1,
          status: "proposed",
          proposal_id: 11,
          scope_kind: "machine",
          scope_id: null,
        }),
      ]),
    );

    await renderWaiting();
    await screen.findByRole("combobox", { name: "Approve into" });
    expect(screen.queryByRole("textbox", { name: "Scope id" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));

    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0][0]).toBe("/proposals/11/approve");
    expect(posts()[0][1]).toEqual({ method: "POST" });
  });

  it("scope chooser disables approve while the id is empty", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([known({ id: 1, status: "proposed", proposal_id: 11 })]),
    );

    await renderWaiting();
    const id = await screen.findByRole("textbox", { name: "Scope id" });
    const approve = screen.getByRole("button", { name: "Approve" });
    expect(approve.hasAttribute("disabled")).toBe(false);

    fireEvent.change(id, { target: { value: "" } });
    expect(approve.hasAttribute("disabled")).toBe(true);
    fireEvent.click(approve);
    expect(posts()).toEqual([]);

    fireEvent.change(id, { target: { value: "nucleos" } });
    expect(approve.hasAttribute("disabled")).toBe(false);
  });

  it("scope chooser clears the id when the kind moves off the declared one", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({
          id: 1,
          status: "proposed",
          proposal_id: 11,
          scope_kind: "project",
          scope_id: "nucleos",
        }),
      ]),
    );

    await renderWaiting();
    const kind = await screen.findByRole("combobox", { name: "Approve into" });
    const approve = screen.getByRole("button", { name: "Approve" });

    fireEvent.change(kind, { target: { value: "agent" } });
    expect((screen.getByRole("textbox", { name: "Scope id" }) as HTMLInputElement).value).toBe("");
    expect(approve.hasAttribute("disabled")).toBe(true);
    fireEvent.click(approve);
    expect(posts()).toEqual([]);

    fireEvent.change(kind, { target: { value: "project" } });
    expect((screen.getByRole("textbox", { name: "Scope id" }) as HTMLInputElement).value).toBe(
      "nucleos",
    );
    expect(approve.hasAttribute("disabled")).toBe(false);
  });

  it("scope chooser names the row it decides", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, status: "proposed", proposal_id: 11, title: "First lesson" }),
        known({ id: 2, status: "proposed", proposal_id: 12, title: "Second lesson" }),
      ]),
    );

    await renderWaiting();
    const first = await screen.findByRole("group", { name: "Scope for First lesson" });
    const second = screen.getByRole("group", { name: "Scope for Second lesson" });
    expect(first.querySelector("select")).not.toBeNull();
    expect(second.querySelector("select")).not.toBeNull();
  });
});
