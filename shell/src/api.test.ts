import { afterEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import type {
  AwaitingRun,
  ClassTally,
  FeedEntry,
  ProjectSummary,
  Proposal,
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
      { project_id: "alpha", mode: "shadow", pending: 2 },
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
    expectGetCall(
      3,
      `${DAEMON_URL}/feed?project_id=alpha%2Fbeta%20%26%20gamma`,
    );

    await expect(api.getFeed(TOKEN)).resolves.toBeNull();
    expectGetCall(4, `${DAEMON_URL}/feed`);
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

  it("approves a proposal and returns the resume run id, null when non-ok", async () => {
    fetchMock
      .mockResolvedValueOnce(okJson({ resume_run_id: 77 }))
      .mockResolvedValueOnce(nonOk());

    await expect(api.approveProposal(TOKEN, 5)).resolves.toBe(77);
    expectPostCall(1, `${DAEMON_URL}/proposals/5/approve`);
    await expect(api.approveProposal(TOKEN, 5)).resolves.toBeNull();
    expectPostCall(2, `${DAEMON_URL}/proposals/5/approve`);
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
    ).resolves.toEqual({ ok: false, status: 422 });
    expectPostCall(3, `${DAEMON_URL}/autopilot/state`, {
      project_id: "alpha",
      mode: "off",
    });
  });
});
