import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import {
  daemonFetch,
  daemonState,
  daemonText,
  project,
  renderApp,
  type DaemonState,
} from "../test/harness";

/** The State mode of one project, over a daemon holding exactly these facts. */
async function openEstado(overrides: Partial<DaemonState> = {}) {
  const state = daemonState({
    projects: [
      project({
        project_id: "nucleos",
        mode: "shadow",
        project_root: "C:/Projects/nucleos",
        root_exists: true,
      }),
    ],
    ...overrides,
  });
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  const rendered = await renderApp({ initialPath: "/projects/nucleos/state" });
  return { state, rendered };
}

/** The delete control, opened. */
async function openDelete() {
  fireEvent.click(await screen.findByRole("button", { name: "delete this folder…" }));
  return within(await screen.findByRole("group", { name: "Delete nucleos's folder" }));
}

/** Arm it the only way it can be armed. */
function typeTheName(panel: ReturnType<typeof within>, name: string) {
  fireEvent.change(panel.getByLabelText("Type nucleos to confirm"), { target: { value: name } });
}

describe("deleting a project's folder", () => {
  /**
   * **The only irreversible thing this app does, and the whole of the interlock is typing the
   * name.**
   *
   * `ConfirmButton`'s arm-then-confirm is the app's interlock for actions whose two labels are all
   * there is to read. This has a path, a count of what exists nowhere else, and a field — stacking
   * both would be theatre, and a third gesture teaches people the gestures are the ritual rather
   * than the thought.
   */
  it("stays disabled until the project's own name is typed", async () => {
    const { state } = await openEstado();
    const panel = await openDelete();

    const button = () => panel.getByRole("button", { name: "delete the folder" });
    expect(button()).toHaveProperty("disabled", true);

    // A near miss is still a miss. Nothing here is a prefix match.
    typeTheName(panel, "nucleo");
    expect(button()).toHaveProperty("disabled", true);

    typeTheName(panel, "nucleos");
    await waitFor(() => expect(button()).toHaveProperty("disabled", false));
    fireEvent.click(button());
    await waitFor(() => expect(state.folderDeleted.length).toBe(1));
    expect(state.folderDeleted[0]).toEqual({ projectId: "nucleos", forgetHistory: false });
  });

  /**
   * **The history is a separate loss, and the default is the same as everywhere else.**
   *
   * Deleting a folder does not imply forgetting what the project did. Two losses, two decisions,
   * and the owner's standing one is that history stays unless somebody says otherwise.
   */
  it("keeps the history unless the box is ticked", async () => {
    const { state } = await openEstado();
    const panel = await openDelete();

    fireEvent.click(panel.getByRole("checkbox"));
    typeTheName(panel, "nucleos");
    fireEvent.click(panel.getByRole("button", { name: "delete the folder" }));

    await waitFor(() => expect(state.folderDeleted.length).toBe(1));
    expect(state.folderDeleted[0].forgetHistory).toBe(true);
  });

  /**
   * **A folder git knows nothing about is the most dangerous case there is**, and it is the one a
   * naive panel would reassure somebody about. `only_here: null` is not a missing reading — it says
   * there are no commits, no remote and nowhere else, so the sentence the other branch offers does
   * not apply. Printing `0 uncommitted` over it would be the reassurance that is exactly backwards.
   */
  it("says a folder git does not know is only there, rather than reporting zero", async () => {
    await openEstado({
      folder: {
        root: "C:/Projects/notes",
        exists: true,
        only_here: null,
        blocked: null,
        holds: { slots: 0, worktrees: 0 },
      },
    });
    const panel = await openDelete();

    expect(await panel.findByText(/not a git repository/)).toBeTruthy();
    expect(panel.queryByText(/not committed/)).toBeNull();
  });

  /**
   * A repository with no remote has no elsewhere at all, which is a different sentence from a count.
   * `unpushed: null` reported as `0` would say *every commit is on a remote* about a repository that
   * has none.
   */
  it("distinguishes a repository with no remote from one that is fully pushed", async () => {
    const local = await openEstado({
      folder: {
        root: "C:/Projects/nucleos",
        exists: true,
        only_here: { uncommitted: 12, unpushed: null },
        blocked: null,
        holds: { slots: 0, worktrees: 0 },
      },
    });
    let panel = await openDelete();
    expect(await panel.findByText(/12 files are not committed/)).toBeTruthy();
    expect(panel.getByText(/no remote, so every commit in it is only there/)).toBeTruthy();

    local.rendered.unmount();

    await openEstado({
      folder: {
        root: "C:/Projects/nucleos",
        exists: true,
        only_here: { uncommitted: 0, unpushed: 0 },
        blocked: null,
        holds: { slots: 0, worktrees: 0 },
      },
    });
    panel = await openDelete();
    expect(await panel.findByText(/Every commit is on a remote/)).toBeTruthy();
  });

  /**
   * **The daemon's standing refusals, said before anything is typed.**
   *
   * A path too near the top of a disk is not a thing that clears by waiting, so the control says so
   * and stays off. Making somebody type a name in order to be told the answer was always no is the
   * shape this exists to avoid.
   */
  it("says why a folder will not be deleted, and does not arm", async () => {
    const { state } = await openEstado({
      folder: {
        root: "C:/",
        exists: true,
        only_here: null,
        blocked: {
          refusal: "root_too_big",
          detail: "C:/ is too near the top of a disk for this app to delete",
        },
        holds: { slots: 0, worktrees: 0 },
      },
    });
    const panel = await openDelete();

    expect(await panel.findByText(/too near the top of a disk/)).toBeTruthy();
    typeTheName(panel, "nucleos");
    expect(panel.getByRole("button", { name: "delete the folder" })).toHaveProperty(
      "disabled",
      true,
    );
    expect(state.folderDeleted).toEqual([]);
  });

  /**
   * Work in flight is the refusal that clears on its own, and the panel says which it is: deleting
   * a directory under a running agent is how a machine ends up with half a repository and a process
   * still writing into it.
   */
  it("will not arm while work is still running in that folder", async () => {
    await openEstado({
      folder: {
        root: "C:/Projects/nucleos",
        exists: true,
        only_here: { uncommitted: 0, unpushed: 0 },
        blocked: null,
        holds: { slots: 1, worktrees: 2 },
      },
    });
    const panel = await openDelete();

    expect(await panel.findByText(/1 slot in flight and 2 worktrees checked out/)).toBeTruthy();
    typeTheName(panel, "nucleos");
    expect(panel.getByRole("button", { name: "delete the folder" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  /** A folder that has already gone has nothing to delete, and the panel offers no button for it. */
  it("says there is nothing to delete when the folder is already gone", async () => {
    await openEstado({
      folder: {
        root: "C:/Projects/moved",
        exists: false,
        only_here: null,
        blocked: null,
        holds: { slots: 0, worktrees: 0 },
      },
    });
    const panel = await openDelete();

    expect(await panel.findByText(/not on this disk any more/)).toBeTruthy();
    typeTheName(panel, "nucleos");
    expect(panel.getByRole("button", { name: "delete the folder" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  /**
   * The kill switch covers this, and the refusal says so in the switch's own words.
   *
   * Everything else the switch stops is autonomous; this is a person pressing a button. It is also
   * the largest thing this app can do, and an emergency stop that a folder deletion sailed past
   * would be the one exception nobody would expect.
   */
  it("reports the kill switch rather than a generic failure", async () => {
    await openEstado({
      folderRefusal: { status: 423, code: "kill_switch", detail: "stopped" },
    });
    const panel = await openDelete();

    typeTheName(panel, "nucleos");
    fireEvent.click(panel.getByRole("button", { name: "delete the folder" }));
    expect(await panel.findByText(/kill switch is engaged — nothing was deleted/)).toBeTruthy();
  });

  /**
   * **Nothing about this is fetched before it is asked for.**
   *
   * Two git subprocesses and a `stat` sit behind that reading, and the State mode is a page people
   * leave open. Reading it to draw a button nobody pressed would run git against a working tree on
   * every visit.
   */
  it("asks the daemon nothing about the folder until the control is opened", async () => {
    // The `vi.hoisted` mock is one object for the whole file, so its call list carries every
    // request the tests above made — all of which opened the panel. Cleared here rather than in a
    // `beforeEach`, because this is the only test in the file that reads the list at all.
    daemon.apiFetch.mockClear();
    await openEstado();
    await screen.findByRole("button", { name: "delete this folder…" });

    const asked = () => daemon.apiFetch.mock.calls.some((call) => String(call[0]).endsWith("/folder"));
    expect(asked()).toBe(false);

    await openDelete();
    await waitFor(() => expect(asked()).toBe(true));
  });

  /**
   * **Focus is handed over, and handed back.**
   *
   * Opening replaces the button that had focus and cancelling removes the panel that had it, so
   * both used to drop focus to `<body>` in the middle of the one irreversible thing this app does.
   * Opening lands on the warning, which is the first thing to read; closing lands back on the
   * button that opened it; and Escape closes, like every other disclosure here.
   */
  it("moves focus to the warning on opening and back to the trigger on cancel", async () => {
    await openEstado();
    const panel = await openDelete();

    await waitFor(() =>
      expect(document.activeElement?.textContent).toMatch(/^This deletes .* Nothing here can undo it\.$/),
    );

    fireEvent.click(panel.getByRole("button", { name: "cancel" }));
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "delete this folder…" })),
    );
  });

  it("closes on Escape and hands focus back", async () => {
    await openEstado();
    const panel = await openDelete();

    fireEvent.keyDown(panel.getByLabelText("Type nucleos to confirm"), { key: "Escape" });
    await waitFor(() =>
      expect(screen.queryByRole("group", { name: "Delete nucleos's folder" })).toBeNull(),
    );
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "delete this folder…" }));
  });
});
