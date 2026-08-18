import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import type {
  AwaitingRun,
  Budget,
  BudgetConfigInput,
  ClassTally,
  FeedEntry,
  ProjectSummary,
  Proposal,
  ScopedKill,
  ShadowDecision,
} from "./api";

const DAEMON_URL = "http://127.0.0.1:8791";
const TOKEN = "test-token";
const fetchMock = vi.fn();

vi.stubGlobal("fetch", fetchMock);

afterEach(() => {
  fetchMock.mockReset();
});

function okJson(payload: unknown) {
  return { ok: true, status: 200, json: async () => payload };
}

function nonOk(status = 422) {
  return { ok: false, status };
}

function expectGetCall(call: number, url: string) {
  const [calledUrl, init] = fetchMock.mock.calls[call - 1] as [
    string,
    RequestInit | undefined,
  ];

  expect(calledUrl).toBe(url);
  expect(init?.method ?? "GET").toBe("GET");
  expect(init?.headers).toEqual({ Authorization: `Bearer ${TOKEN}` });
  expect(init?.body).toBeUndefined();
}

function expectPostCall(
  call: number,
  url: string,
  body?: Record<string, unknown>,
) {
  const init: RequestInit = {
    method: "POST",
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      "Content-Type": "application/json",
    },
  };
  if (body !== undefined) init.body = JSON.stringify(body);

  expect(fetchMock).toHaveBeenNthCalledWith(call, url, init);
}

describe("daemon API client", () => {
  it("gets project summaries and returns null for a non-ok response", async () => {
    const projects = [
      { project_id: "alpha", mode: "shadow", project_root: null, pending: 2, classes_ready: 0, classes_total: 0, promotable: false, open_proposals: 0, wip_limit: 3, queue_full: false },
    ] satisfies ProjectSummary[];
    fetchMock
      .mockResolvedValueOnce(okJson(projects))
      .mockResolvedValueOnce(nonOk());

    await expect(api.getProjects(TOKEN)).resolves.toEqual(projects);
    expectGetCall(1, `${DAEMON_URL}/projects`);
    await expect(api.getProjects(TOKEN)).resolves.toBeNull();
    expectGetCall(2, `${DAEMON_URL}/projects`);
  });

  it("gets the feed for its default, all, and encoded project scopes", async () => {
    const feed = [
      {
        id: 7,
        project_id: "alpha/beta",
        kind: "run.completed",
        summary: "Run completed",
        run_id: 19,
        created_at: "2026-07-20T10:00:00Z",
      },
    ] satisfies FeedEntry[];
    fetchMock
      .mockResolvedValueOnce(okJson(feed))
      .mockResolvedValueOnce(okJson(feed))
      .mockResolvedValueOnce(okJson(feed))
      .mockResolvedValueOnce(nonOk());

    await expect(api.getFeed(TOKEN)).resolves.toEqual(feed);
    expectGetCall(1, `${DAEMON_URL}/feed`);

    await expect(api.getFeed(TOKEN, { scope: "all" })).resolves.toEqual(feed);
    expectGetCall(2, `${DAEMON_URL}/feed?scope=all`);

    await expect(
      api.getFeed(TOKEN, { projectId: "alpha/beta & gamma" }),
    ).resolves.toEqual(feed);
    // `+` for the space, the way `URLSearchParams` writes it and the way the runs filter has
    // always written it. Axum decodes a query as form-urlencoded, so this is the same string on
    // the daemon's side as `%20` — one encoder for every filter, rather than two conventions.
    expectGetCall(
      3,
      `${DAEMON_URL}/feed?project_id=alpha%2Fbeta+%26+gamma`,
    );

    await expect(api.getFeed(TOKEN)).resolves.toBeNull();
    expectGetCall(4, `${DAEMON_URL}/feed`);
  });

  it("sends only the search fields that were filled in", async () => {
    fetchMock.mockResolvedValueOnce(okJson([])).mockResolvedValueOnce(okJson([]));

    // The daemon switches from listing a scope to searching it on the PRESENCE of any search
    // field, so an empty string is not the same as an absent one — sending `q=` would turn a plain
    // "show me the feed" into a search for nothing.
    await api.getFeed(TOKEN, { scope: "all", q: "", kind: "" });
    expectGetCall(1, `${DAEMON_URL}/feed?scope=all`);

    await api.getFeed(TOKEN, {
      projectId: "alpha", q: "gate failed", kind: "worktree_gate_failed",
      since: "2026-07-01T00:00:00Z", limit: 200,
    });
    expectGetCall(
      2,
      `${DAEMON_URL}/feed?project_id=alpha&q=gate+failed&kind=worktree_gate_failed`
        + `&since=2026-07-01T00%3A00%3A00Z&limit=200`,
    );
  });

  it("gets an encoded project's scoreboard and returns null when non-ok", async () => {
    const scoreboard = [
      {
        mode: "shadow",
        action_class: "filesystem.write",
        total: 8,
        would_allow: 3,
        would_pend: 4,
        would_deny: 1,
        reviewed: 5,
        agree: 4,
        disagree: 1,
      },
    ] satisfies ClassTally[];
    fetchMock
      .mockResolvedValueOnce(okJson(scoreboard))
      .mockResolvedValueOnce(nonOk());

    await expect(api.getScoreboard(TOKEN, "alpha/beta")).resolves.toEqual(
      scoreboard,
    );
    expectGetCall(1, `${DAEMON_URL}/scoreboard?project_id=alpha%2Fbeta`);
    await expect(api.getScoreboard(TOKEN, "alpha/beta")).resolves.toBeNull();
    expectGetCall(2, `${DAEMON_URL}/scoreboard?project_id=alpha%2Fbeta`);
  });

  it("gets an encoded project's shadow decisions and returns null when non-ok", async () => {
    const decisions = [
      {
        id: 3,
        run_id: 11,
        tool_name: "write_file",
        tool_input: null,
        decision: "pend",
        reason: "Needs approval",
        action_class: "filesystem.write",
        classifier_version: 1,
        human_verdict: null,
        reviewed_at: null,
        created_at: "2026-07-20T10:00:00Z",
      },
    ] satisfies ShadowDecision[];
    fetchMock
      .mockResolvedValueOnce(okJson(decisions))
      .mockResolvedValueOnce(nonOk());

    await expect(
      api.getShadowDecisions(TOKEN, "alpha & beta"),
    ).resolves.toEqual(decisions);
    expectGetCall(
      1,
      `${DAEMON_URL}/shadow-decisions?project_id=alpha%20%26%20beta`,
    );
    await expect(
      api.getShadowDecisions(TOKEN, "alpha & beta"),
    ).resolves.toBeNull();
    expectGetCall(
      2,
      `${DAEMON_URL}/shadow-decisions?project_id=alpha%20%26%20beta`,
    );
  });

  it("posts an explicit verdict and reflects response ok", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({}))
      .mockResolvedValueOnce(nonOk());

    await expect(api.setVerdict(TOKEN, 42, "approve")).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/shadow-decisions/42/verdict`, {
      verdict: "approve",
    });
    await expect(api.setVerdict(TOKEN, 42, "reject")).resolves.toBe(false);
    expectPostCall(2, `${DAEMON_URL}/shadow-decisions/42/verdict`, {
      verdict: "reject",
    });
  });

  it("gets awaiting-approval runs and returns null when non-ok", async () => {
    const runs = [
      {
        id: 21,
        project_id: null,
        prompt: "Update the release notes",
        cwd: null,
        created_at: "2026-07-20T10:00:00Z",
      },
    ] satisfies AwaitingRun[];
    fetchMock
      .mockResolvedValueOnce(okJson(runs))
      .mockResolvedValueOnce(nonOk());

    await expect(api.getAwaitingApproval(TOKEN)).resolves.toEqual(runs);
    expectGetCall(1, `${DAEMON_URL}/runs/awaiting-approval`);
    await expect(api.getAwaitingApproval(TOKEN)).resolves.toBeNull();
    expectGetCall(2, `${DAEMON_URL}/runs/awaiting-approval`);
  });

  it("posts a worktree release and reflects response ok", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({}))
      .mockResolvedValueOnce(nonOk());

    await expect(api.releaseWorktree(TOKEN, 21)).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/worktrees/21/release`);
    await expect(api.releaseWorktree(TOKEN, 21)).resolves.toBe(false);
    expectPostCall(2, `${DAEMON_URL}/worktrees/21/release`);
  });

  it("gets pending proposals and returns null for a non-ok response", async () => {
    const proposals = [
      {
        id: 5,
        kind: "action-approval",
        status: "pending",
        run_id: 41,
        session_id: "session-1",
        project_id: "alpha",
        tool_name: "Bash",
        reasoning: "The command needs explicit approval",
        tool_input: '{"command":"npm test"}',
        created_at: "2026-07-20T10:00:00Z",
        decided_at: null,
      },
    ] satisfies Proposal[];
    fetchMock
      .mockResolvedValueOnce(okJson(proposals))
      .mockResolvedValueOnce(nonOk());

    await expect(api.getProposals(TOKEN)).resolves.toEqual(proposals);
    expectGetCall(1, `${DAEMON_URL}/proposals`);
    await expect(api.getProposals(TOKEN)).resolves.toBeNull();
    expectGetCall(2, `${DAEMON_URL}/proposals`);
  });

  it("approves a proposal and returns the resume run id", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ resume_run_id: 77 }));

    await expect(api.approveProposal(TOKEN, 5)).resolves.toEqual({
      ok: true,
      resumeRunId: 77,
      // Null and not absent: an ordinary approval closed nothing early, and the field being there
      // with nothing in it is what lets a caller test it without knowing which kind it approved.
      closed: null,
    });
    expectPostCall(1, `${DAEMON_URL}/proposals/5/approve`);
  });

  // The refusal carries the daemon's sentence, because a 409 here is either "already decided" or
  // "can never resume" and the panel has to say which. A test on `ok: false` alone would pass
  // against the version that threw the sentence away.
  it("carries the daemon's own reason when an approval is refused", async () => {
    fetchMock.mockResolvedValueOnce({
      ok: false,
      status: 409,
      text: async () => "this approval cannot resume the run: no live worktree for the paused run",
    });

    await expect(api.approveProposal(TOKEN, 5)).resolves.toEqual({
      ok: false,
      status: 409,
      reason:
        "this approval cannot resume the run: no live worktree for the paused run",
    });
  });

  it("falls back to a sentence of its own when a refusal carries no body", async () => {
    fetchMock.mockResolvedValueOnce({ ok: false, status: 401, text: async () => "" });

    await expect(api.approveProposal(TOKEN, 5)).resolves.toEqual({
      ok: false,
      status: 401,
      reason: "The daemon refused this approval and gave no reason.",
    });
  });

  it("rejects a proposal and reflects response ok", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({}))
      .mockResolvedValueOnce(nonOk());

    await expect(api.rejectProposal(TOKEN, 21)).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/proposals/21/reject`);
    await expect(api.rejectProposal(TOKEN, 21)).resolves.toBe(false);
    expectPostCall(2, `${DAEMON_URL}/proposals/21/reject`);
  });

  it("gets the kill-switch engaged field and returns null when non-ok", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ engaged: true }))
      .mockResolvedValueOnce(nonOk());

    await expect(api.getKillSwitch(TOKEN)).resolves.toBe(true);
    expectGetCall(1, `${DAEMON_URL}/autopilot/kill`);
    await expect(api.getKillSwitch(TOKEN)).resolves.toBeNull();
    expectGetCall(2, `${DAEMON_URL}/autopilot/kill`);
  });

  it("posts the kill-switch state and reflects response ok", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({}))
      .mockResolvedValueOnce(nonOk());

    await expect(api.setKillSwitch(TOKEN, true)).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/autopilot/kill`, { engaged: true });
    await expect(api.setKillSwitch(TOKEN, false)).resolves.toBe(false);
    expectPostCall(2, `${DAEMON_URL}/autopilot/kill`, { engaged: false });
  });

  it("sets project mode, omits an undefined root, and preserves non-ok status", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({}))
      .mockResolvedValueOnce(okJson({}))
      .mockResolvedValueOnce(nonOk(422));

    await expect(
      api.setProjectMode(TOKEN, "alpha", "active", "C:/Projects/alpha"),
    ).resolves.toEqual({ ok: true });
    expectPostCall(1, `${DAEMON_URL}/autopilot/state`, {
      project_id: "alpha",
      mode: "active",
      project_root: "C:/Projects/alpha",
    });

    await expect(
      api.setProjectMode(TOKEN, "alpha", "shadow"),
    ).resolves.toEqual({ ok: true });
    expectPostCall(2, `${DAEMON_URL}/autopilot/state`, {
      project_id: "alpha",
      mode: "shadow",
    });

    await expect(
      api.setProjectMode(TOKEN, "alpha", "off"),
    ).resolves.toEqual({ ok: false, fault: "failed", status: 422 });
    expectPostCall(3, `${DAEMON_URL}/autopilot/state`, {
      project_id: "alpha",
      mode: "off",
    });
  });

  it("tells a refused token apart from an unreachable daemon", async () => {
    fetchMock
      .mockResolvedValueOnce(nonOk(401))
      .mockRejectedValueOnce(new TypeError("Failed to fetch"));

    // A stale token and a stopped daemon are the two failures a user can act
    // on, and they need opposite actions — so they must not read alike.
    await expect(api.setProjectMode(TOKEN, "alpha", "shadow")).resolves.toEqual({
      ok: false,
      fault: "unauthorized",
      status: 401,
    });
    await expect(api.setProjectMode(TOKEN, "alpha", "shadow")).resolves.toEqual({
      ok: false,
      fault: "unreachable",
      status: 0,
    });
  });

  it("reads the daemon status line, and says why when it cannot", async () => {
    fetchMock
      .mockResolvedValueOnce({ ok: true, status: 200, text: async () => "idle · 0 runs" })
      .mockResolvedValueOnce(nonOk(403))
      .mockResolvedValueOnce(nonOk(500))
      .mockRejectedValueOnce(new TypeError("Failed to fetch"));

    await expect(api.getStatus(TOKEN)).resolves.toEqual({ ok: true, value: "idle · 0 runs" });
    expectGetCall(1, `${DAEMON_URL}/status`);
    // 403 is unauthorized too: from here, both mean the token buys nothing.
    await expect(api.getStatus(TOKEN)).resolves.toEqual({ ok: false, fault: "unauthorized", status: 403 });
    await expect(api.getStatus(TOKEN)).resolves.toEqual({ ok: false, fault: "failed", status: 500 });
    await expect(api.getStatus(TOKEN)).resolves.toEqual({ ok: false, fault: "unreachable", status: 0 });
  });

  it("gets the budget and returns null for a non-ok response", async () => {
    const budget = {
      limit_usd: 50,
      period: "monthly",
      hourly_limit_usd: null,
      per_run_reserve_usd: 0.5,
      time_cost_per_hour_usd: 3,
      window_spend_usd: 12.5,
      hourly_spend_usd: 1,
      paused: false,
      reason: null,
    } satisfies Budget;
    fetchMock
      .mockResolvedValueOnce(okJson(budget))
      .mockResolvedValueOnce(nonOk());

    await expect(api.getBudget(TOKEN)).resolves.toEqual(budget);
    expectGetCall(1, `${DAEMON_URL}/autopilot/budget`);
    await expect(api.getBudget(TOKEN)).resolves.toBeNull();
    expectGetCall(2, `${DAEMON_URL}/autopilot/budget`);
  });

  it("posts a budget config and returns the refreshed budget, null when non-ok", async () => {
    const config = {
      limit_usd: 50,
      period: "monthly",
      hourly_limit_usd: null,
      per_run_reserve_usd: 0.5,
      time_cost_per_hour_usd: 3,
    } satisfies BudgetConfigInput;
    const refreshed = {
      ...config,
      window_spend_usd: 0,
      hourly_spend_usd: 0,
      paused: false,
      reason: null,
    } satisfies Budget;
    fetchMock
      .mockResolvedValueOnce(okJson(refreshed))
      .mockResolvedValueOnce(nonOk());

    await expect(api.setBudget(TOKEN, config)).resolves.toEqual(refreshed);
    expectPostCall(1, `${DAEMON_URL}/autopilot/budget`, config);
    await expect(api.setBudget(TOKEN, config)).resolves.toBeNull();
    expectPostCall(2, `${DAEMON_URL}/autopilot/budget`, config);
  });

  it("gets scoped kill switches and returns null for a non-ok response", async () => {
    const scoped = [
      { scope_type: "project", scope_id: "alpha", engaged: true },
    ] satisfies ScopedKill[];
    fetchMock
      .mockResolvedValueOnce(okJson(scoped))
      .mockResolvedValueOnce(nonOk());

    await expect(api.getScopedKills(TOKEN)).resolves.toEqual(scoped);
    expectGetCall(1, `${DAEMON_URL}/autopilot/kill/scoped`);
    await expect(api.getScopedKills(TOKEN)).resolves.toBeNull();
    expectGetCall(2, `${DAEMON_URL}/autopilot/kill/scoped`);
  });

  it("posts a scoped kill and reflects response ok", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({}))
      .mockResolvedValueOnce(nonOk());

    await expect(
      api.setScopedKill(TOKEN, "project", "alpha", true),
    ).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/autopilot/kill/scoped`, {
      scope_type: "project",
      scope_id: "alpha",
      engaged: true,
    });
    await expect(
      api.setScopedKill(TOKEN, "trigger", "scheduled", false),
    ).resolves.toBe(false);
    expectPostCall(2, `${DAEMON_URL}/autopilot/kill/scoped`, {
      scope_type: "trigger",
      scope_id: "scheduled",
      engaged: false,
    });
  });
});

describe("runs, presets and the assistant", () => {
  it("sends only the filters that were set, and omits the empty ones", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));

    await api.getRuns(TOKEN, { limit: 50 });
    expectGetCall(1, `${DAEMON_URL}/runs?limit=50`);
  });

  it("encodes filter values rather than pasting them into the query", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));

    await api.getRuns(TOKEN, { projectId: "alpha/beta", q: "a b&c", limit: 10 });
    expectGetCall(
      1,
      `${DAEMON_URL}/runs?project_id=alpha%2Fbeta&q=a+b%26c&limit=10`,
    );
  });

  it("asks for everything when no filter is given", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));

    await api.getRuns(TOKEN);
    expectGetCall(1, `${DAEMON_URL}/runs`);
  });

  it("reports WHY a run was refused, since a 503 and a 500 need different answers", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ id: 12 }))
      .mockResolvedValueOnce(nonOk(503))
      .mockResolvedValueOnce(nonOk(401));

    const input = { prompt: "go", project_id: null, cwd: null, mode: "real", steerable: false };
    await expect(api.createRun(TOKEN, input)).resolves.toEqual({ ok: true, value: 12 });
    await expect(api.createRun(TOKEN, input)).resolves.toEqual({
      ok: false,
      fault: "failed",
      status: 503,
    });
    // 401 is the one fault the shell acts on globally, so it must not be flattened into "failed".
    await expect(api.createRun(TOKEN, input)).resolves.toEqual({
      ok: false,
      fault: "unauthorized",
      status: 401,
    });
  });

  it("keeps the two steering refusals apart, since they mean different things", async () => {
    fetchMock
      .mockResolvedValueOnce({ ok: true, status: 202 })
      .mockResolvedValueOnce(nonOk(409))
      .mockResolvedValueOnce(nonOk(403));

    await expect(api.steerRun(TOKEN, 7, "try the other branch")).resolves.toEqual({
      ok: true,
      value: null,
    });
    expectPostCall(1, `${DAEMON_URL}/runs/7/message`, { message: "try the other branch" });

    // 409 is about this moment — it finished, or it never listened. 403 is about this run for as
    // long as it exists. Collapsing them would let the page offer "try again" for the one where
    // trying again can never work.
    await expect(api.steerRun(TOKEN, 7, "hello")).resolves.toMatchObject({ status: 409 });
    await expect(api.steerRun(TOKEN, 7, "hello")).resolves.toMatchObject({ status: 403 });
  });

  it("closes a conversation with DELETE and treats an already-closed one as done", async () => {
    fetchMock
      .mockResolvedValueOnce({ ok: true, status: 204 })
      .mockResolvedValueOnce(nonOk(404));

    await expect(api.endRunTurns(TOKEN, 7)).resolves.toBe(true);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(`${DAEMON_URL}/runs/7/message`);
    expect(init.method).toBe("DELETE");

    // 404 is the only real failure here: no such run. A channel that was already closed answers
    // 204, because that is the state the caller asked for.
    await expect(api.endRunTurns(TOKEN, 7)).resolves.toBe(false);
  });

  it("asks for a listening run only when told to", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ id: 1 }))
      .mockResolvedValueOnce(okJson({ id: 2 }));

    const base = { prompt: "go", project_id: null, cwd: null, mode: "real" };
    await api.createRun(TOKEN, { ...base, steerable: false });
    await api.createRun(TOKEN, { ...base, steerable: true });

    // Sent explicitly both ways rather than omitted when false: the daemon defaults it to false for
    // callers that predate the field, and relying on that default would make the shell's request
    // depend on a compatibility rule instead of on what the person ticked.
    expectPostCall(1, `${DAEMON_URL}/runs`, { ...base, steerable: false });
    expectPostCall(2, `${DAEMON_URL}/runs`, { ...base, steerable: true });
  });

  it("treats a 404 from cancel as a run that already ended", async () => {
    fetchMock
      .mockResolvedValueOnce({ ok: true, status: 200 })
      .mockResolvedValueOnce(nonOk(404));

    await expect(api.cancelRun(TOKEN, 4)).resolves.toEqual({ ok: true, value: null });
    await expect(api.cancelRun(TOKEN, 4)).resolves.toEqual({
      ok: false, fault: "failed", status: 404,
    });
  });

  it("carries a duplicate preset name back as a 409 rather than a bare failure", async () => {
    const preset = {
      id: 1, name: "nightly", prompt: "sweep", project_id: null, cwd: null,
      mode: "real", created_at: "2026-07-20T10:00:00Z", updated_at: "2026-07-20T10:00:00Z",
    };
    fetchMock
      .mockResolvedValueOnce(okJson(preset))
      .mockResolvedValueOnce(nonOk(409));

    const input = { name: "nightly", prompt: "sweep", project_id: null, cwd: null, mode: "real" };
    await expect(api.createPreset(TOKEN, input)).resolves.toEqual({ ok: true, value: preset });
    expectPostCall(1, `${DAEMON_URL}/presets`, input);
    await expect(api.createPreset(TOKEN, input)).resolves.toEqual({
      ok: false,
      fault: "failed",
      status: 409,
    });
  });

  it("updates a preset with PUT and deletes it with DELETE", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ id: 3 }))
      .mockResolvedValueOnce({ ok: true, status: 204 });

    const input = { name: "n", prompt: "p", project_id: null, cwd: null, mode: "real" };
    await api.updatePreset(TOKEN, 3, input);
    expect(fetchMock.mock.calls[0]?.[0]).toBe(`${DAEMON_URL}/presets/3`);
    expect((fetchMock.mock.calls[0]?.[1] as RequestInit).method).toBe("PUT");

    await expect(api.deletePreset(TOKEN, 3)).resolves.toBe(true);
    expect((fetchMock.mock.calls[1]?.[1] as RequestInit).method).toBe("DELETE");
  });

  it("speaks as the shell's own chat, so it never takes the sidecar's turn slot", async () => {
    fetchMock.mockResolvedValueOnce(okJson({ turn_id: 88 }));

    await expect(
      api.sendAssistantMessage(TOKEN, api.SHELL_CHAT_ID, "hello"),
    ).resolves.toEqual({ ok: true, value: 88 });
    expectPostCall(1, `${DAEMON_URL}/assistant/message`, {
      chat_id: "shell",
      text: "hello",
    });
  });

  it("surfaces a busy chat as 409 rather than as an outage", async () => {
    fetchMock.mockResolvedValueOnce(nonOk(409));

    await expect(
      api.sendAssistantMessage(TOKEN, api.SHELL_CHAT_ID, "hello"),
    ).resolves.toEqual({ ok: false, fault: "failed", status: 409 });
  });
});

describe("inspection, backups, health and keys", () => {
  it("encodes both the project and the path on every inspect route", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson([]))
      .mockResolvedValueOnce({ ok: true, status: 200, text: async () => "x" })
      .mockResolvedValueOnce(okJson([]))
      .mockResolvedValueOnce({ ok: true, status: 200, text: async () => "" });

    const project = "a/b";
    await api.getProjectLs(TOKEN, project, "src/x y");
    expectGetCall(1, `${DAEMON_URL}/projects/a%2Fb/ls?path=src%2Fx%20y`);
    await api.getProjectCat(TOKEN, project, "src/x y");
    expectGetCall(2, `${DAEMON_URL}/projects/a%2Fb/cat?path=src%2Fx%20y`);
    await api.getProjectGrep(TOKEN, project, "a&b", "src");
    expectGetCall(3, `${DAEMON_URL}/projects/a%2Fb/grep?q=a%26b&path=src`);
    await api.getProjectDiff(TOKEN, project);
    expectGetCall(4, `${DAEMON_URL}/projects/a%2Fb/diff`);
  });

  it("defaults ls to the project root", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));

    await api.getProjectLs(TOKEN, "alpha");
    expectGetCall(1, `${DAEMON_URL}/projects/alpha/ls?path=`);
  });

  it("keeps a cat refusal's status, so a 404 root can be told from a 413 file", async () => {
    fetchMock.mockResolvedValueOnce(nonOk(413));

    await expect(api.getProjectCat(TOKEN, "alpha", "big")).resolves.toEqual({
      ok: false,
      fault: "failed",
      status: 413,
    });
  });

  it("takes, lists and stages a restore without confusing the three", async () => {
    const info = { name: "nucleos-2026-07-29.db", migration_version: 12, size_bytes: 4096 };
    fetchMock
      .mockResolvedValueOnce(okJson(info))
      .mockResolvedValueOnce(okJson([info]))
      .mockResolvedValueOnce(okJson({
        name: info.name, migration_version: 12, applies: "on next daemon start",
      }));

    await expect(api.takeBackup(TOKEN)).resolves.toEqual(info);
    expect((fetchMock.mock.calls[0]?.[1] as RequestInit).method).toBe("POST");
    await expect(api.getBackups(TOKEN)).resolves.toEqual([info]);
    expectGetCall(2, `${DAEMON_URL}/backups`);
    // A restore reports WHEN it applies, because nothing has been swapped when this returns.
    await expect(api.restoreBackup(TOKEN, info.name)).resolves.toEqual({
      name: info.name,
      migration_version: 12,
      applies: "on next daemon start",
    });
  });

  it("reads the health readout, subsystems and all", async () => {
    const readout = {
      status: "degraded",
      subsystems: [
        { name: "database", status: "ok" },
        { name: "email_sidecar", status: "down", reason: "not-running" },
      ],
    };
    fetchMock.mockResolvedValueOnce(okJson(readout));

    await expect(api.getHealthReadout(TOKEN)).resolves.toEqual(readout);
    expectGetCall(1, `${DAEMON_URL}/health/readout`);
  });

  it("returns the minted secret once and tells a taken name apart from a refusal", async () => {
    const created = {
      name: "agent-cli", level: "run-creating",
      created_at: "2026-07-29T10:00:00Z", token: "nk_secret",
    };
    fetchMock
      .mockResolvedValueOnce(okJson(created))
      .mockResolvedValueOnce(nonOk(409))
      .mockResolvedValueOnce(nonOk(403));

    await expect(api.createApiToken(TOKEN, "agent-cli", "run-creating")).resolves.toEqual({
      ok: true, value: created,
    });
    expectPostCall(1, `${DAEMON_URL}/api-tokens`, { name: "agent-cli", level: "run-creating" });
    await expect(api.createApiToken(TOKEN, "agent-cli", "read-only")).resolves.toMatchObject({
      ok: false, status: 409,
    });
    // 403 means this token may not mint keys — an authorization fault, not a generic failure.
    await expect(api.createApiToken(TOKEN, "other", "admin")).resolves.toMatchObject({
      ok: false, fault: "unauthorized", status: 403,
    });
  });

  it("encodes a key name on revoke", async () => {
    fetchMock.mockResolvedValueOnce({ ok: true, status: 204 });

    await expect(api.revokeApiToken(TOKEN, "a b/c")).resolves.toBe(true);
    expect(fetchMock.mock.calls[0]?.[0]).toBe(`${DAEMON_URL}/api-tokens/a%20b%2Fc`);
    expect((fetchMock.mock.calls[0]?.[1] as RequestInit).method).toBe("DELETE");
  });
});

describe("mail cursor, requeue and the attention heartbeat", () => {
  it("reads a cursor per mailbox and passes a null through as null", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ uidvalidity: 12, last_uid: 340 }))
      .mockResolvedValueOnce(okJson(null));

    await expect(api.getEmailCursor(TOKEN, "INBOX")).resolves.toEqual({
      uidvalidity: 12, last_uid: 340,
    });
    expectGetCall(1, `${DAEMON_URL}/email/cursor?mailbox=INBOX`);
    // Never collected from is a state the page shows, not a failure it hides.
    await expect(api.getEmailCursor(TOKEN, "INBOX")).resolves.toBeNull();
  });

  it("distinguishes the three requeue refusals, which need three different answers", async () => {
    fetchMock
      .mockResolvedValueOnce({ ok: true, status: 204 })
      .mockResolvedValueOnce(nonOk(404))
      .mockResolvedValueOnce(nonOk(409))
      .mockResolvedValueOnce(nonOk(500));

    await expect(api.requeueEmail(TOKEN, 1)).resolves.toBe(true);
    await expect(api.requeueEmail(TOKEN, 1)).resolves.toBe("unknown");
    await expect(api.requeueEmail(TOKEN, 1)).resolves.toBe("conflict");
    await expect(api.requeueEmail(TOKEN, 1)).resolves.toBe("failed");
  });

  it("tells a send that was never attempted from one that was", async () => {
    const message = { to: "maria@example.com", subject: "Re: the roof", body: "on Tuesday" };
    const refused = (status: number, reason: string) => ({
      ok: false, status, text: async () => reason,
    });
    fetchMock
      .mockResolvedValueOnce({ ok: true, status: 204, text: async () => "" })
      .mockResolvedValueOnce(refused(400, "a recipient must not contain a line break"))
      .mockResolvedValueOnce(refused(503, "no submission host is configured"))
      .mockResolvedValueOnce(refused(502, "the email sidecar could not send the message"))
      .mockRejectedValueOnce(new TypeError("Failed to fetch"));

    await expect(api.sendEmail(TOKEN, message)).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/email/send`, message);

    // 400 and 503 both mean nothing left this process, and each carries the daemon's own sentence
    // because it is the half that names the field, or the file to edit.
    await expect(api.sendEmail(TOKEN, message)).resolves.toEqual({
      kind: "invalid", reason: "a recipient must not contain a line break",
    });
    await expect(api.sendEmail(TOKEN, message)).resolves.toEqual({
      kind: "unconfigured", reason: "no submission host is configured",
    });
    // The one that must never be reported as "not sent": the sidecar was asked.
    await expect(api.sendEmail(TOKEN, message)).resolves.toEqual({
      kind: "undelivered", reason: "the email sidecar could not send the message",
    });
    // No response at all is a stronger statement than a 502, not a weaker one — the request never
    // reached the daemon, so nothing was attempted.
    await expect(api.sendEmail(TOKEN, message)).resolves.toEqual({
      kind: "unreachable", reason: "the daemon is not reachable",
    });
  });

  it("falls back to the status when a refusal carries no sentence", async () => {
    fetchMock.mockResolvedValueOnce({ ok: false, status: 502, text: async () => "" });

    await expect(
      api.sendEmail(TOKEN, { to: "a@b.com", subject: "s", body: "b" }),
    ).resolves.toEqual({ kind: "undelivered", reason: "HTTP 502" });
  });

  it("beats globally by default and scopes to a project when asked", async () => {
    fetchMock
      .mockResolvedValueOnce({ ok: true, status: 204 })
      .mockResolvedValueOnce({ ok: true, status: 204 });

    await expect(api.sendAttentionHeartbeat(TOKEN)).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/autopilot/attention`, {});
    await api.sendAttentionHeartbeat(TOKEN, "alpha");
    expectPostCall(2, `${DAEMON_URL}/autopilot/attention`, { project_id: "alpha" });
  });
});

/** The routes that had no client at all until the shell grew pages for them. */
describe("the git queue, skipped items and jobs started by hand", () => {
  it("lists every git request, and narrows to one project when asked", async () => {
    fetchMock.mockResolvedValueOnce(okJson([])).mockResolvedValueOnce(okJson([]));

    await api.listVcsRequests(TOKEN);
    expectGetCall(1, `${DAEMON_URL}/vcs/requests`);
    await api.listVcsRequests(TOKEN, "alpha");
    expectGetCall(2, `${DAEMON_URL}/vcs/requests?project_id=alpha`);
  });

  /**
   * The bare route, never `/wait`. Both are the same read; `/wait` holds the connection open for
   * `vcs::DEFAULT_WAIT`, which a page that already polls would spend for nothing.
   */
  it("reads one request without holding the connection open", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson({ id: 3, status: "failed", result_sha: null, failure_reason: "conflict" }),
    );

    await expect(api.getVcsRequest(TOKEN, 3)).resolves.toEqual({
      id: 3, status: "failed", result_sha: null, failure_reason: "conflict",
    });
    expectGetCall(1, `${DAEMON_URL}/vcs/requests/3`);
  });

  /** Its own door: `/reject` guards on `action-approval` and would answer 409 for these rows. */
  it("dismisses a skipped item through dismiss, not reject", async () => {
    fetchMock.mockResolvedValueOnce({ ok: true, status: 204 });

    await expect(api.dismissSkippedItem(TOKEN, 4)).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/proposals/4/dismiss`);
  });

  it("reads the skipped items from their own list", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));

    await expect(api.getSkippedItems(TOKEN)).resolves.toEqual([]);
    expectGetCall(1, `${DAEMON_URL}/proposals/skipped-items`);
  });

  /**
   * Blank means "the house limit governs" and "one round", which is what `null` says on the wire.
   * Sending `0` would say something else entirely — a job that may spend nothing and run no rounds.
   */
  it("sends null for a budget and a round count left blank", async () => {
    fetchMock.mockResolvedValueOnce({ ok: true, status: 201, json: async () => ({ job_id: 9 }) });

    await expect(
      api.createJob(TOKEN, { projectId: "alpha", prompt: "tidy the logs" }),
    ).resolves.toEqual({ ok: true, jobId: 9 });
    expectPostCall(1, `${DAEMON_URL}/jobs`, {
      project_id: "alpha", prompt: "tidy the logs", budget_usd: null, max_rounds: null,
    });
  });

  it("passes a budget and a round count through when they are given", async () => {
    fetchMock.mockResolvedValueOnce({ ok: true, status: 201, json: async () => ({ job_id: 10 }) });

    await api.createJob(TOKEN, {
      projectId: "alpha", prompt: "tidy the logs", budgetUsd: 2.5, maxRounds: 3,
    });
    expectPostCall(1, `${DAEMON_URL}/jobs`, {
      project_id: "alpha", prompt: "tidy the logs", budget_usd: 2.5, max_rounds: 3,
    });
  });

  /**
   * The status alone cannot identify this refusal: `409` is both "the kill switch is engaged" and
   * "no room", and those have different remedies. The daemon writes a sentence; it must survive.
   */
  it("carries the daemon's own words back for a refusal", async () => {
    fetchMock.mockResolvedValueOnce({
      ok: false,
      status: 409,
      text: async () => "the kill switch is engaged; nothing autonomous starts",
    });

    await expect(
      api.createJob(TOKEN, { projectId: "alpha", prompt: "go" }),
    ).resolves.toEqual({
      ok: false,
      status: 409,
      reason: "the kill switch is engaged; nothing autonomous starts",
    });
  });

  it("falls back to a sentence of its own when a refusal carries none", async () => {
    fetchMock.mockResolvedValueOnce({ ok: false, status: 503, text: async () => "  " });

    await expect(
      api.createJob(TOKEN, { projectId: "alpha", prompt: "go" }),
    ).resolves.toEqual({
      ok: false, status: 503, reason: "The daemon refused this job and gave no reason.",
    });
  });
});

describe("moving an occurrence, reading the web, and the dictations", () => {
  /** The daemon rejects a move with no length, so the duration always travels with it. */
  it("sends the occurrence, where it is going, and how long it runs", async () => {
    fetchMock.mockResolvedValueOnce({ ok: true, status: 204 });

    await expect(
      api.moveCalendarOccurrence(TOKEN, 5, "2026-08-03T09:00:00", "2026-08-04T14:00:00", 30),
    ).resolves.toBe(true);
    expectPostCall(1, `${DAEMON_URL}/calendar/events/5/move`, {
      occurrence_local: "2026-08-03T09:00:00",
      to_local: "2026-08-04T14:00:00",
      duration_minutes: 30,
    });
  });

  it("searches the web through the daemon rather than a provider directly", async () => {
    const view = { cached: [], provider: "brave", results: [] };
    fetchMock.mockResolvedValueOnce(okJson(view));

    await expect(api.searchWeb(TOKEN, "rust sqlite")).resolves.toEqual({ ok: true, value: view });
    expectPostCall(1, `${DAEMON_URL}/web/search`, { query: "rust sqlite", limit: null });
  });

  /** A pillar switched off is a 503 the caller can act on, never collapsed into a null. */
  it("reports a disabled pillar as its own status rather than as nothing", async () => {
    fetchMock.mockResolvedValueOnce(nonOk(503));

    await expect(api.searchWeb(TOKEN, "anything")).resolves.toEqual({
      ok: false, fault: "failed", status: 503,
    });
  });

  it("reads a page by URL and keeps the requester off the wire", async () => {
    const view = { id: 1, from_cache: false };
    fetchMock.mockResolvedValueOnce(okJson(view));

    await api.readWebPage(TOKEN, "https://example.com/a");
    // No `requester` field: the daemon derives it from owner presence, and a field here would be a
    // permission the caller grants itself.
    expectPostCall(1, `${DAEMON_URL}/web/read`, { url: "https://example.com/a" });
  });

  it("reads the dictations from their own route, apart from the memos", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));

    await expect(api.listVoiceDictations(TOKEN)).resolves.toEqual([]);
    expectGetCall(1, `${DAEMON_URL}/voice/dictations`);
  });

  it("asks the daemon for capacity, and reads a failure as absent rather than as empty", async () => {
    const readout = { house: { limit: 3, held: 1 }, projects: [] };
    fetchMock.mockResolvedValueOnce(okJson(readout));

    await expect(api.getConcurrency(TOKEN)).resolves.toEqual(readout);
    expectGetCall(1, `${DAEMON_URL}/concurrency`);

    fetchMock.mockResolvedValueOnce(nonOk(500));
    await expect(api.getConcurrency(TOKEN)).resolves.toBeNull();
  });

  it("asks for live work without the recency window", async () => {
    fetchMock.mockResolvedValue(okJson([]));

    await api.getJobs(TOKEN, undefined, { live: true });
    await api.getLiveRuns(TOKEN);

    expectGetCall(1, `${DAEMON_URL}/jobs?live=true`);
    expectGetCall(2, `${DAEMON_URL}/runs?live=true&limit=${api.LIVE_LIST_LIMIT}`);
  });

  it("keeps the old job listing byte for byte when nothing asks for live", async () => {
    fetchMock.mockResolvedValue(okJson([]));

    await api.getJobs(TOKEN);
    await api.getJobs(TOKEN, "alpha");

    expectGetCall(1, `${DAEMON_URL}/jobs`);
    expectGetCall(2, `${DAEMON_URL}/jobs?project_id=alpha`);
  });
});

describe("readIdeConversation", () => {
  it("reads back what was said in a conversation had in the editor", async () => {
    fetchMock.mockResolvedValueOnce(
      okJson([
        { by_owner: true, text: "arranja o parser" },
        { by_owner: false, text: "está arranjado" },
      ]),
    );

    const said = await api.readIdeConversation(TOKEN, "aaaa-1111");

    expectGetCall(1, `${DAEMON_URL}/assistant/ide-sessions/aaaa-1111`);
    expect(said).toEqual([
      { by_owner: true, text: "arranja o parser" },
      { by_owner: false, text: "está arranjado" },
    ]);
  });

  it("reads back nothing when the transcript is no longer on this machine", async () => {
    fetchMock.mockResolvedValueOnce(nonOk(404));

    // Null and not an empty list. An empty list is a conversation nobody spoke in, and the window
    // says so out loud; a transcript that was not found is not something to make that claim about.
    expect(await api.readIdeConversation(TOKEN, "aaaa-1111")).toBeNull();
  });

  it("sends the id encoded, because it names a path on the daemon", async () => {
    fetchMock.mockResolvedValueOnce(okJson([]));

    await api.readIdeConversation(TOKEN, "../elsewhere/secret");

    expectGetCall(1, `${DAEMON_URL}/assistant/ide-sessions/..%2Felsewhere%2Fsecret`);
  });
});
