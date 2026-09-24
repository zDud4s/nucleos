// §spec workspace-de-projeto
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
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
  heldSlots,
  project,
  readings,
  renderApp,
  slot,
  type DaemonState,
} from "../test/harness";

/**
 * The State mode over a daemon holding exactly these facts, with some routes made to fail.
 *
 * `failing` is read on every call rather than captured, so a test can let a route answer once and
 * then break it — which is the only way to reach "stale": a last good read, then a refusal.
 */
async function openState(state: DaemonState, failing: Set<string> = new Set()) {
  const answer = daemonFetch(state);
  daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
    for (const prefix of failing) {
      if (path.startsWith(prefix)) throw new Error(`${prefix} is down`);
    }
    return answer(path, init);
  });
  daemon.apiText.mockImplementation(daemonText(state));
  daemon.probeHealth.mockResolvedValue(true);
  return renderApp({ initialPath: "/projects/nucleos/state" });
}

function settingsState(overrides: Partial<ReturnType<typeof project>> = {}): DaemonState {
  return daemonState({
    projects: [project({ project_id: "nucleos", mode: "shadow", project_root: "C:/p", ...overrides })],
  });
}

const css = () =>
  readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "ui.css"), "utf8");

describe("the mode switch says which mode is set", () => {
  /**
   * The pressed segment used to paint `--surface-raised` inside an `Inset` that is also Raised, so
   * the setting differed from its neighbours by text colour alone while the green offer beside it
   * read as selected. jsdom applies no stylesheet, so the sheet is where this is asserted.
   */
  it("marks the setting with a shape, and steps down a rung inside an inset", () => {
    const sheet = css();
    const pressed = /\.ui-switch-seg\[aria-pressed="true"\]\s*\{([^}]*)\}/.exec(sheet);
    expect(pressed?.[1]).toMatch(/box-shadow:\s*inset\s+0\s+-2px\s+0\s+var\(--text\)/);

    const inInset = /\.ui-panel-inset\s+\.ui-switch-seg\[aria-pressed="true"\]\s*\{([^}]*)\}/.exec(
      sheet,
    );
    expect(inInset?.[1]).toMatch(/background:\s*var\(--surface-sunken\)/);
  });

  it("keeps the offer to act outline-only until it is armed", () => {
    const rule =
      /\.ui-switch-seg-wrap\s*>\s*\.ui-confirm:not\(\.ui-confirm-armed\)\s*>\s*\.ui-button-approve:not\(:disabled\):not\(\[aria-disabled="true"\]\)\s*\{([^}]*)\}/.exec(
        css(),
      );
    expect(rule?.[1]).toMatch(/background:\s*none/);
    expect(rule?.[1]).toMatch(/var\(--tone-active-border\)/);
  });

  /**
   * Pressing a segment makes it the setting, and the setting is inert. With native `disabled` the
   * focus that pressed it fell to `<body>`; the block now keeps the tab stop.
   */
  it("keeps focus on the segment that was pressed once it becomes the setting", async () => {
    const state = settingsState();
    await openState(state);

    const off = await screen.findByRole("button", { name: "Turn off" });
    off.focus();
    fireEvent.click(off);

    await waitFor(() => expect(off.getAttribute("aria-pressed")).toBe("true"));
    expect(off.getAttribute("aria-disabled")).toBe("true");
    expect((off as HTMLButtonElement).disabled).toBe(false);
    expect(document.activeElement).toBe(off);
  });

  it("a locked offer stays reachable and is described by why it is locked", async () => {
    await openState(settingsState({ promotable: false, classes_ready: 2, classes_total: 5 }));

    const locked = await screen.findByRole("button", { name: "Let it act" });
    expect(locked.getAttribute("aria-disabled")).toBe("true");
    const reason = document.getElementById(locked.getAttribute("aria-describedby") ?? "");
    expect(reason?.textContent).toBe("3 of 5 action classes are still short of the bar.");
  });

  it("the three blocks are headings a screen reader can walk to", async () => {
    await openState(settingsState());
    for (const name of ["Mode", "Open-proposal ceiling", "Action classes"]) {
      expect(await screen.findByRole("heading", { level: 3, name })).toBeTruthy();
    }
  });
});

describe("the open-proposal ceiling", () => {
  /** The house pattern for a control that cannot be pressed now: reachable, described, inert. */
  it("an unavailable step is aria-disabled, described by the reading, and does nothing", async () => {
    const state = settingsState({ wip_limit: null, open_review_items: 2 });
    await openState(state);

    const clear = await screen.findByRole("button", { name: "no ceiling" });
    expect((clear as HTMLButtonElement).disabled).toBe(false);
    expect(clear.getAttribute("aria-disabled")).toBe("true");

    const reading = document.getElementById(clear.getAttribute("aria-describedby") ?? "");
    expect(reading?.textContent).toBe("no ceiling — 2 proposals waiting on you");
    expect(reading?.getAttribute("aria-live")).toBe("polite");

    fireEvent.click(clear);
    expect(state.projects[0].wip_limit).toBe(null);
  });

  it("keeps focus on the step that lowered the ceiling to zero", async () => {
    const state = settingsState({ wip_limit: 1, open_review_items: 0 });
    await openState(state);

    const lower = await screen.findByRole("button", { name: "Lower the ceiling" });
    lower.focus();
    fireEvent.click(lower);

    expect(await screen.findByText("0 of 0 taken")).toBeTruthy();
    expect(lower.getAttribute("aria-disabled")).toBe("true");
    expect(document.activeElement).toBe(lower);
  });

  it("says a write failed instead of leaving the old number to speak for it", async () => {
    await openState(settingsState({ wip_limit: 3 }), new Set(["/projects/nucleos/wip-limit"]));

    fireEvent.click(await screen.findByRole("button", { name: "Raise the ceiling" }));
    expect(await screen.findByText(/The ceiling was not changed/)).toBeTruthy();
  });

  it("names what it bounds, so it cannot be read as the worktree ceiling", async () => {
    await openState(settingsState({ wip_limit: 3 }));
    expect(await screen.findByText(/How many proposals may wait on you/)).toBeTruthy();
  });
});

describe("the action classes", () => {
  it("leads with the núcleo's verdict and labels every tally, ids in mono", async () => {
    const state = daemonState({
      projects: [
        project({
          project_id: "nucleos",
          mode: "shadow",
          project_root: "C:/p",
          classes_ready: 5,
          classes_total: 5,
        }),
      ],
      scoreboard: [
        {
          mode: "shadow",
          action_class: "read-local",
          total: 46,
          would_allow: 40,
          would_pend: 5,
          would_deny: 1,
          reviewed: 18,
          agree: 12,
          disagree: 6,
        },
      ],
    });
    await openState(state);

    expect(await screen.findByText("5 of 5 classes clear the bar.")).toBeTruthy();

    const table = screen.getByRole("table", { name: "Shadow decisions by action class" });
    const headers = within(table)
      .getAllByRole("columnheader")
      .map((cell) => cell.textContent);
    expect(headers).toEqual(["Class", "Decided", "Reviewed", "Disagreed"]);

    const id = within(table).getByRole("rowheader", { name: "read-local" });
    expect(id.className).toContain("font-mono");
    const cells = within(id.closest("tr") as HTMLElement)
      .getAllByRole("cell")
      .map((cell) => cell.textContent);
    expect(cells).toEqual(["46", "18", "6"]);

    // Nothing behind a hover any more, and nothing drawn in the badge shape.
    expect(table.querySelector("[title]")).toBeNull();
    expect(table.querySelector(".rounded-pill")).toBeNull();
  });
});

describe("the readings", () => {
  it("lead with the gate, say its failures in words, and label the gauge", async () => {
    const state = daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow" })],
      readings: readings({
        efficiency: {
          measured_runs: 41,
          unmeasured_runs: 0,
          median_total_tokens: 128_000,
          previous_median_total_tokens: null,
        },
        cost: { usd: 13.16, runs: 41 },
        gate: { passed: 22, failed: 3, errored: 1, no_gate: 15 },
        delivered: { landed: 9, timed: 7, median_minutes: 34 },
      }),
    });
    const { container } = await openState(state);

    await screen.findByText("85%");
    const readingsSection = container.querySelector('section[aria-label="Readings"]') as HTMLElement;
    const order = Array.from(readingsSection.querySelectorAll("article")).map((card) =>
      card.getAttribute("aria-label"),
    );
    expect(order).toEqual(["Gate", "Delivered", "Cost", "Token efficiency"]);

    expect(screen.getByText(/3 failed · 1 could not run · 15 ungated/).className).toContain(
      "ui-wrong",
    );
    expect(
      screen.getByRole("img", { name: "22 passed, 3 failed, 1 could not run, of 26 judged" }),
    ).toBeTruthy();

    // Read once and not polled, so it says when.
    expect(within(readingsSection).getByText(/^Last 30 days, read at \d\d:\d\d\.$/)).toBeTruthy();
  });

  it("a refused read says so once, rather than four em dashes that look like four answers", async () => {
    await openState(
      daemonState({ projects: [project({ project_id: "nucleos", mode: "shadow" })] }),
      new Set(["/projects/nucleos/readings"]),
    );

    expect(await screen.findByText(/did not answer for this project.s readings/)).toBeTruthy();
    expect(screen.queryByText("—")).toBeNull();
  });
});

describe("occupancy", () => {
  it("counts the worktree slots in words and draws them as a list", async () => {
    const state = daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow" })],
      concurrency: {
        house: { limit: 4, held: 1 },
        projects: [
          heldSlots("nucleos", 3, [slot({ project_id: "nucleos", owner_kind: "job", owner_id: 24 })]),
        ],
      },
    });
    await openState(state);

    expect(await screen.findByText("1 of 3 worktree slots in use")).toBeTruthy();
    const list = screen.getByRole("list", { name: "Worktree slots" });
    const items = within(list).getAllByRole("listitem");
    expect(items).toHaveLength(3);
    // The visible words are the name. A label on a generic box is ignored by several readers.
    for (const item of items) expect(item.getAttribute("aria-label")).toBeNull();
    expect(within(list).getAllByText("free")).toHaveLength(2);
  });

  it("says a refused read is refused, not still reading", async () => {
    await openState(
      daemonState({ projects: [project({ project_id: "nucleos", mode: "shadow" })] }),
      new Set(["/concurrency"]),
    );
    expect(await screen.findByText(/did not say which worktree slots are in use/)).toBeTruthy();
    expect(screen.queryByText("Reading capacity…")).toBeNull();
  });
});

describe("branches", () => {
  /**
   * A poll that fails after a good one keeps the branches and dates them. Before, the whole panel
   * turned into "its folder may have moved", which is the wrong sentence for a missed poll.
   */
  it("keeps the last good branches and says how old they are", async () => {
    const failing = new Set<string>();
    const state = daemonState({
      projects: [project({ project_id: "nucleos", mode: "shadow" })],
      branches: {
        integration: "trunk",
        branches: [
          {
            name: "trunk",
            ahead: 0,
            behind: 0,
            measured: true,
            last_commit_at: "2026-08-23T09:00:00Z",
            last_subject: "the trunk moved",
          },
        ],
        omitted: 0,
      },
    });
    const { queryClient } = await openState(state, failing);
    expect(await screen.findByText("trunk")).toBeTruthy();

    failing.add("/projects/nucleos/branches");
    await queryClient.refetchQueries({ predicate: (query) => JSON.stringify(query.queryKey).includes("branches") });

    expect(await screen.findByText(/view is stale — last good read/)).toBeTruthy();
    expect(screen.getByText("trunk")).toBeTruthy();
    expect(screen.queryByText(/its folder may have moved/)).toBeNull();
  });
});
