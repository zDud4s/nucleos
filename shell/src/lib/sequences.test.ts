// @vitest-environment node
import { describe, expect, it } from "vitest";
import type { FeedEntry } from "../data/feed";
import {
  LANE_FOLD_ABOVE,
  buildSequences,
  newestException,
  sequenceKey,
  sequenceProgressAt,
  sequenceStateAt,
  traceLanes,
} from "./sequences";

const MINUTE = 60_000;
const T0 = Date.UTC(2026, 8, 16, 21, 0);

function line(id: number, minute: number, kind: string, overrides: Partial<FeedEntry> = {}): FeedEntry {
  return {
    id,
    project_id: "alpha",
    kind,
    summary: `line ${id}`,
    run_id: null,
    subject: null,
    created_at: new Date(T0 + minute * MINUTE).toISOString(),
    ...overrides,
  };
}

describe("which lines are one sequence", () => {
  it("keys on the subject, then the run, then the line itself", () => {
    expect(sequenceKey(line(1, 0, "job_started", { subject: "job:57", run_id: 9 }))).toBe("job:57");
    expect(sequenceKey(line(2, 0, "run_retry", { run_id: 900612 }))).toBe("run:900612");
    expect(sequenceKey(line(4, 0, "email_urgent", { project_id: null }))).toBe("kind:email_urgent");
  });

  it("groups subject-less lines of one kind into a series: marks, no bar, never open", () => {
    const [series, lone] = buildSequences([
      line(1, 0, "email_digest", { project_id: null }),
      line(2, 60, "command_finished", { project_id: "alpha" }),
      line(3, 1440, "email_digest", { project_id: null }),
      line(4, 2880, "email_digest", { project_id: null }),
    ]);
    expect(series.family).toBe("series");
    expect(series.name).toBe("e-mail digest");
    expect(series.lines).toHaveLength(3);
    expect(series.open).toBe(false);
    expect(series.owner).toBeNull();
    expect(sequenceStateAt(series, T0 + 30 * MINUTE, T0 + 3000 * MINUTE)).toBe("settled");
    // A series of one is simply the line.
    expect(lone.family).toBe("line");
    expect(lone.name).toBe("project command finished");
    expect(lone.owner).toBe("alpha");
  });

  it("a series with different owners has none, and one with a failure in it stays a row in a dense lane", () => {
    const [mixed] = buildSequences([
      line(1, 0, "config_written", { project_id: "alpha" }),
      line(2, 1, "config_written", { project_id: "bravo" }),
    ]);
    expect(mixed.owner).toBeNull();
    const rows = [
      ...Array.from({ length: LANE_FOLD_ABOVE + 1 }, (_, i) => line(i + 1, i, "job_finished", { subject: `job:${i + 1}` })),
      line(90, 50, "job_plan_failed", { subject: null }),
      line(91, 55, "job_plan_failed", { subject: null }),
    ];
    const [jobs] = traceLanes(buildSequences(rows));
    expect(jobs.shown.map((sequence) => sequence.key)).toEqual(["kind:job_plan_failed"]);
  });

  it("groups a job's lines into one sequence, whatever order they arrive in", () => {
    const [job] = buildSequences([
      line(3, 7, "job_gate_failed", { subject: "job:57", run_id: 900609 }),
      line(1, 0, "job_started", { subject: "job:57", summary: "job 57 started on job/57-importer from the rule nightly reconciliation" }),
      line(4, 44, "job_item_conflicted", { subject: "job:57" }),
      line(2, 3, "job_planned", { subject: "job:57" }),
    ]);
    expect(job.lines.map((row) => row.id)).toEqual([1, 2, 3, 4]);
    expect(job.name).toBe("job 57 · nightly reconciliation");
    expect(job.owner).toBe("alpha");
    expect(job.start).toBe(T0);
    expect(job.last).toBe(T0 + 44 * MINUTE);
    // Worst is the failed gate; how it ended is the item that did not merge, and that is its colour.
    expect(job.gravity).toBe("wrong");
    expect(job.shade).toBe("held");
    expect(job.says).toBe("job item did not merge");
    expect(job.ending).toBe("item did not merge");
    expect(job.open).toBe(false);
    expect(job.lane).toBe("jobs");
  });

  it("names each family, and a lone line by its kind", () => {
    const names = buildSequences([
      line(1, 0, "run_failed_final", { subject: "run:900598" }),
      line(2, 1, "shadow_run_completed", { subject: "run:900590" }),
      line(3, 2, "council_finished", { subject: "council:12", project_id: null }),
      line(4, 3, "team_run_finished", { subject: "team_run:8", project_id: null }),
      line(5, 4, "vcs_request_finished", { subject: "vcs:21" }),
      line(7, 6, "promotion_ready", { project_id: "charlie" }),
      line(8, 7, "job_finished", { subject: "widget:3" }),
    ]).map((sequence) => sequence.name);
    expect(names).toEqual([
      "run 900598",
      "shadow run 900590",
      "council 12",
      "team run 8",
      "vcs request 21",
      "promotion ready",
      "widget:3",
    ]);
  });

  it("counts attempts from retries, and a retry that is the newest line leaves the run open", () => {
    const [failed, retrying] = buildSequences([
      line(1, 0, "run_retry", { subject: "run:900598" }),
      line(2, 8, "run_retry", { subject: "run:900598" }),
      line(3, 16, "run_failed_final", { subject: "run:900598" }),
      line(4, 20, "run_retry", { subject: "run:900612" }),
    ]);
    expect(failed.attempts).toBe(3);
    expect(failed.ending).toBe("failed for good");
    expect(failed.open).toBe(false);
    expect(failed.lane).toBe("runs");
    expect(retrying.attempts).toBe(2);
    expect(retrying.open).toBe(true);
  });

  it("a parked job is open and says what it waits for; a start with no subject is never open", () => {
    const [parked, bare] = buildSequences([
      line(1, 0, "job_waiting", { subject: "job:58", summary: "job 58 is waiting: another run holds the project's worktree slot" }),
      line(2, 1, "job_started"),
    ]);
    expect(parked.open).toBe(true);
    expect(parked.says).toBe("waiting for a slot");
    expect(bare.open).toBe(false);
  });

  it("takes the lane of the line that weighs most, else of the first line", () => {
    const [mixed, plain] = buildSequences([
      line(1, 0, "worktree_run_completed", { subject: "run:7" }),
      line(2, 1, "land_resolution_failed", { subject: "run:7" }),
      line(3, 2, "vcs_resolution_started", { subject: "vcs:9" }),
      line(4, 3, "worktree_released", { subject: "vcs:9" }),
    ]);
    expect(mixed.lane).toBe("git");
    expect(plain.lane).toBe("git");
  });

  it("a shadow decision shades its bar violet even though it is routine", () => {
    const [shadow] = buildSequences([line(1, 0, "shadow_run_completed", { subject: "run:900590" })]);
    expect(shadow.gravity).toBe("routine");
    expect(shadow.shade).toBe("shadow");
    expect(shadow.ending).toBe("completed");
  });

  it("is coloured by how it ended, not by the last thing that went wrong", () => {
    const [recovered] = buildSequences([
      line(1, 0, "job_started", { subject: "run:5" }),
      line(2, 10, "job_item_failed", { subject: "run:5" }),
      line(3, 20, "web.read", { subject: "run:5" }),
    ]);
    expect(recovered.gravity).toBe("wrong");
    expect(recovered.shade).toBe("routine");
    expect(recovered.ending).toBe("web page read");
  });
});

describe("a sequence at a moment of the replay", () => {
  const now = T0 + 120 * MINUTE;
  const [closed, open] = buildSequences([
    line(1, 10, "job_started", { subject: "job:1" }),
    line(2, 50, "job_finished", { subject: "job:1" }),
    line(3, 60, "job_waiting", { subject: "job:2" }),
  ]);

  it("is queued before its first line, running until its last, settled after", () => {
    expect(sequenceStateAt(closed, T0, now)).toBe("queued");
    expect(sequenceStateAt(closed, T0 + 30 * MINUTE, now)).toBe("running");
    expect(sequenceStateAt(closed, T0 + 50 * MINUTE, now)).toBe("settled");
    expect(sequenceProgressAt(closed, T0 + 30 * MINUTE, now)).toBeCloseTo(0.5);
    expect(sequenceProgressAt(closed, now, now)).toBe(1);
  });

  it("an open one runs from its first line to now, and fills toward now", () => {
    expect(sequenceStateAt(open, T0 + 59 * MINUTE, now)).toBe("queued");
    expect(sequenceStateAt(open, now, now)).toBe("running");
    expect(sequenceProgressAt(open, T0 + 90 * MINUTE, now)).toBeCloseTo(0.5);
  });
});

describe("lanes on the trace", () => {
  it("leaves empty lanes out and keeps the lanes' own order", () => {
    const lanes = traceLanes(
      buildSequences([
        line(1, 0, "email_urgent", { project_id: null }),
        line(2, 1, "job_finished", { subject: "job:1" }),
      ]),
    );
    expect(lanes.map((lane) => lane.lane)).toEqual(["jobs", "mail"]);
    expect(lanes[0].folded).toEqual([]);
  });

  it("folds a dense lane's routine and keeps every exception and every open sequence as a row", () => {
    const rows = Array.from({ length: LANE_FOLD_ABOVE + 3 }, (_, i) =>
      line(i + 1, i, i === 4 ? "job_failed" : i === 9 ? "job_waiting" : "job_finished", { subject: `job:${i + 1}` }),
    );
    const [jobs] = traceLanes(buildSequences(rows));
    expect(jobs.sequences).toHaveLength(LANE_FOLD_ABOVE + 3);
    // The failure, and the job still parked: an open sequence is never folded away.
    expect(jobs.shown.map((sequence) => sequence.key)).toEqual(["job:5", "job:10"]);
    expect(jobs.folded).toHaveLength(LANE_FOLD_ABOVE + 1);
    expect(jobs.lines).toBe(LANE_FOLD_ABOVE + 3);
  });

  it("selects the newest sequence that went wrong, was held or asks, and none on a quiet night", () => {
    const sequences = buildSequences([
      line(1, 0, "job_gate_failed", { subject: "job:1" }),
      line(2, 5, "email_urgent", { project_id: null }),
      line(3, 9, "job_finished", { subject: "job:3" }),
    ]);
    expect(newestException(sequences)?.key).toBe("kind:email_urgent");
    expect(newestException(buildSequences([line(4, 0, "job_finished")]))).toBeNull();
  });
});
