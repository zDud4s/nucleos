import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Rules from "./Rules";
import type { ProjectRules } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function rules(overrides: Partial<ProjectRules> = {}): ProjectRules {
  return {
    project_id: "alpha", project_root: "C:/repos/alpha",
    rules_file: "present", rules_error: null, gate_command: null,
    schedules: [], repo_triggers: [],
    wip_limit: null, open_proposals: 0, queue_full: false,
    ...overrides,
  };
}

function schedule(overrides: Record<string, unknown> = {}) {
  return {
    name: "nightly", cron: "0 3 * * *", prompt: "sweep the repo",
    cwd: null, timezone: null,
    next_fire_at: "2026-08-01T03:00:00+00:00", problem: null,
    last_fired_at: null, fires_today: 0, daily_cap: 24,
    ...overrides,
  };
}

function daemonReturning(body: ProjectRules) {
  fetchMock.mockImplementation(async (url: string) => {
    if (String(url).includes("/rules")) return { ok: true, status: 200, json: async () => body };
    return { ok: true, status: 204 };
  });
}

async function settle() {
  await act(async () => {});
}

describe("what a project does on its own", () => {
  beforeEach(() => fetchMock.mockReset());
  afterEach(() => fetchMock.mockReset());

  it("names a rule that can never fire instead of listing it like the rest", async () => {
    daemonReturning(
      rules({
        schedules: [
          schedule({ name: "broken", problem: "'nightly' is not a cron expression", next_fire_at: null }),
          schedule({ name: "healthy" }),
        ],
      }),
    );

    render(<Rules token="t" projectId="alpha" />);
    await settle();

    // The daemon skips such a rule and logs at debug twice a minute, so the rule sits in the file
    // looking exactly like one that works. This is the only place it says otherwise.
    expect(screen.getByText("'nightly' is not a cron expression")).toBeTruthy();
    expect(screen.getByText("never fires")).toBeTruthy();
    expect(screen.getByText("armed")).toBeTruthy();
  });

  it("reports an unreadable rules file rather than an empty list of rules", async () => {
    daemonReturning(
      rules({ rules_file: "unreadable", rules_error: "unknown field `schedule`" }),
    );

    render(<Rules token="t" projectId="alpha" />);
    await settle();

    // `deny_unknown_fields` makes a misspelt key an error precisely so it is not read as "no
    // rules" — and a project with a typo must not look like a project with nothing scheduled.
    expect(screen.getByText(/could not read/)).toBeTruthy();
    expect(screen.getByText("unknown field `schedule`")).toBeTruthy();
  });

  it("tells a project with no gate that nothing checks its work", async () => {
    daemonReturning(rules({ gate_command: null }));

    render(<Rules token="t" projectId="alpha" />);
    await settle();

    expect(screen.getByText(/nothing checks the work before you do/)).toBeTruthy();
  });

  it("separates a trigger that is arming from one that is watching", async () => {
    daemonReturning(
      rules({
        repo_triggers: [
          { name: "on-main", branch: "main", prompt: "review", last_sha: null },
          { name: "on-dev", branch: "dev", prompt: "review", last_sha: "abcdef1234567890" },
        ],
      }),
    );

    render(<Rules token="t" projectId="alpha" />);
    await settle();

    // A trigger with no baseline fires nothing, which is not the same as a broken one — the next
    // commit it sees becomes the baseline, and the one after that fires it.
    expect(screen.getByText(/the next one becomes its baseline/)).toBeTruthy();
    expect(screen.getByText("abcdef1234")).toBeTruthy();
  });

  it("says a full queue is why nothing is starting", async () => {
    daemonReturning(rules({ wip_limit: 3, open_proposals: 3, queue_full: true }));

    render(<Rules token="t" projectId="alpha" />);
    await settle();

    expect(screen.getByText(/New autonomous work is deferred until you review one/)).toBeTruthy();
    // The brake is self-clearing, which is what separates it from the budget — a person reading
    // "deferred" needs to know the way out is reviewing, not waiting.
    expect(screen.getByText(/releases itself the moment you review/)).toBeTruthy();
  });

  it("sets a ceiling, and clears it back to none", async () => {
    daemonReturning(rules({ wip_limit: 3, open_proposals: 1 }));
    render(<Rules token="t" projectId="alpha" />);
    await settle();

    fireEvent.change(screen.getByDisplayValue("3"), { target: { value: "5" } });
    fireEvent.click(screen.getByRole("button", { name: "Set ceiling" }));
    await settle();

    const writes = fetchMock.mock.calls.filter(([url]) => String(url).includes("/wip-limit"));
    expect(JSON.parse(String((writes[0]?.[1] as RequestInit).body))).toEqual({ limit: 5 });

    fireEvent.click(screen.getByRole("button", { name: "Remove the ceiling" }));
    await settle();
    const cleared = fetchMock.mock.calls.filter(([url]) => String(url).includes("/wip-limit"));
    // null, not 0. A ceiling of zero would defer everything forever; no ceiling is a different
    // state and the column already expresses it.
    expect(JSON.parse(String((cleared[1]?.[1] as RequestInit).body))).toEqual({ limit: null });
  });

  it("refuses to send a ceiling that is not a whole number", async () => {
    daemonReturning(rules({ wip_limit: 3 }));
    render(<Rules token="t" projectId="alpha" />);
    await settle();

    fireEvent.change(screen.getByDisplayValue("3"), { target: { value: "2.5" } });
    await settle();

    expect(screen.getByRole("button", { name: "Set ceiling" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByText(/whole number/)).toBeTruthy();
  });
});
