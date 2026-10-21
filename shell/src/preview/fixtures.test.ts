// @vitest-environment node
import { describe, expect, it } from "vitest";
import { READINESS_MIN_AGREE_PERCENT, READINESS_MIN_REVIEWED, RESOLVE_MIN_REVIEWED } from "../data/autopilot";
import { previewPipeline, judgeResolveFixture, JUDGE_RESOLUTIONS, JUDGE_RESOLVE_STATUS, FEED, FEED_SEEN, FEED_TIMELINE, JUDGE_STATUS, JUDGE_VERDICTS, NOTES_GRAPH, CHANNELS, CHANNEL_IDENTITIES, CHANNEL_NOTIFY_POLICY, NOW, OWNER_NOTES, PAIRING_CODE, PAIRING_STATUS, answer, PROJECTS, SCOREBOARD, VCS_REQUESTS } from "./daemon";
import { sidecarKeyOf } from "../data/system";
import { mergePolicy } from "../data/feed";
import type { MachineConfig } from "../data/machine-config";
import { readEfficiencySignal, readFeedKind, waitReasonFromSummary } from "../data/feed";
import { LANE_FOLD_ABOVE, buildSequences, traceLanes } from "../lib/sequences";
import { quietGaps } from "../lib/timeline";
import { feedGravityOf, feedLaneOf } from "../ui";

/**
 * The states the preview must be able to photograph, pinned as facts about the fixtures.
 *
 * Not a test of the app — nothing under `src/` imports the preview daemon, and this file is the
 * only thing in the suite that does. It exists because a fixture is the only reason a shot shows
 * anything, and a state with no fixture is a state nobody has ever seen: the third mode segment
 * was disabled on all four projects, so no shot had ever photographed it enabled, and the armed
 * label that wrapped and grew the roster row shipped unseen. A missing fixture fails silently by
 * construction — the daemon answers `[]` for a route it has no branch for, and an empty panel is
 * a legitimate state — so the claim has to be made here, out loud.
 */
describe("the preview fixtures", () => {
  it("the pipeline answer fills every block, every window, with one project red in the global view", () => {
    for (const raw of ["7", "30", "90", null]) {
      const days = raw === null ? 30 : Number(raw);
      const report = previewPipeline(null, raw);
      expect(report.days).toBe(days);
      if ("error" in report.landing || "error" in report.gate || "error" in report.postgate) throw new Error("block");
      if ("error" in report.jobs || report.pressure === null || "error" in report.pressure) throw new Error("block");
      expect(report.landing.daily).toHaveLength(days);
      expect(report.gate.duration_ms_daily).toHaveLength(days);
      expect(report.postgate.red_s_daily).toHaveLength(days);
      expect(report.jobs.daily).toHaveLength(days);
      expect(report.pressure.daily).toHaveLength(days);
      expect(report.postgate.red_now).toBe(true);
      expect(report.postgate.per_project.some((row) => row.red_now)).toBe(true);
      expect(report.landing.conflicts).not.toBeNull();
      expect(report.gate.cache).not.toBeNull();
    }
    const scoped = previewPipeline("alpha", "7");
    expect(scoped.scope).toBe("project");
    expect("error" in scoped.postgate ? true : scoped.postgate.red_now).toBe(false);
  });

  it("the feed serves lines across kinds, and exactly one the shell cannot read", () => {
    expect(FEED.length).toBeGreaterThanOrEqual(14);
    expect(FEED.filter((row) => readFeedKind(row.kind) === null).map((row) => row.kind)).toEqual(["map_stamp_recorded"]);
    for (const kind of ["job_finished", "job_started", "job_failed", "vcs_request_finished", "email_urgent", "token_efficiency", "web.read", "team_run_finished", "team_trigger_armed"]) expect(FEED.some((row) => row.kind === kind), kind).toBe(true);
    expect(new Set(FEED.map((row) => readFeedKind(row.kind)?.tone)).size).toBeGreaterThanOrEqual(4);
  });

  it("no badge restates its own row", () => {
    // The P3 of the 2026-09-09 critique, held as a property of the fixture rather than as taste:
    // six of twelve pairs were the same string, because the summaries had been written from the
    // labels. A summary in the núcleo's shape carries an id, a path or a branch; the badge carries
    // the class, which is also the page's filter facet.
    for (const row of FEED) {
      const label = readFeedKind(row.kind)?.label ?? row.kind;
      expect(row.summary.trim().toLowerCase(), row.kind).not.toBe(label.toLowerCase());
    }
  });

  it("the rows that carry a second reading are written so it can be read", () => {
    // Both readings are parsed out of the summary, so a generic fixture sentence silently
    // switches the device off. Neither had ever been photographed.
    const waiting = FEED.find((row) => row.kind === "job_waiting");
    expect(waitReasonFromSummary(waiting!.summary)).toBe("slot");
    const efficiency = FEED.find((row) => row.kind === "token_efficiency");
    expect(readEfficiencySignal(efficiency!.summary)?.signal).toBe("cold cache");
  });
  /*
    The time axis has something to be read in, in every state its shots point at.

    Each claim below is a picture that would otherwise be empty or a lie: the verdict needs all
    three exceptional gravities after the marker, the trace needs a silence long enough to be named
    and sequences still open at now, the seven-day shot needs a lane dense enough to fold its
    routine sequences into one row, and the incremental poll needs ids that grow with time the way
    the núcleo's row ids do.
  */
  it("the timeline holds a night worth a verdict, a named silence and a dense week", () => {
    const lookedAt = Date.parse(FEED_SEEN.seen_at ?? "");
    const since = FEED_TIMELINE.filter((row) => Date.parse(row.created_at) > lookedAt);
    const gravities = since.map((row) => feedGravityOf(row.kind));
    for (const gravity of ["wrong", "held", "asks"] as const) {
      expect(gravities.filter((g) => g === gravity).length, gravity).toBe(2);
    }

    const night = FEED_TIMELINE.filter((row) => Date.parse(row.created_at) > NOW - 15 * 3_600_000);
    expect(quietGaps(night.map((row) => Date.parse(row.created_at))).length).toBeGreaterThan(0);

    // The night's sequences are whole: job 57 is one row of four lines, run 900598 one of three
    // attempts, and two are still going at now — a parked job and a run between attempts.
    const nightly = buildSequences(since);
    expect(nightly.find((sequence) => sequence.key === "job:57")?.lines).toHaveLength(4);
    expect(nightly.find((sequence) => sequence.key === "run:900598")?.attempts).toBe(3);
    expect(nightly.filter((sequence) => sequence.open).map((sequence) => sequence.key).sort()).toEqual(["job:58", "run:900612"]);

    const week = FEED_TIMELINE.filter((row) => Date.parse(row.created_at) > NOW - 7 * 86_400_000);
    const dense = traceLanes(buildSequences(week)).filter((lane) => lane.sequences.length > LANE_FOLD_ABOVE);
    expect(dense.map((lane) => lane.lane)).toContain("jobs");
    // And the fold keeps something back as a row: the job that failed on Wednesday.
    expect(dense.find((lane) => lane.lane === "jobs")?.shown.length).toBeGreaterThan(0);
    expect(new Set(week.map((row) => feedLaneOf(row.kind))).size).toBe(6);
    // A week-old line with no subject is a line of its own; the work itself always carries one.
    for (const row of week) {
      if (row.kind.startsWith("job_") || row.kind.startsWith("run_") || row.kind.startsWith("council_")) expect(row.subject, row.kind).not.toBeNull();
    }

    for (let i = 1; i < FEED_TIMELINE.length; i += 1) {
      expect(FEED_TIMELINE[i].id).toBeGreaterThan(FEED_TIMELINE[i - 1].id);
      expect(Date.parse(FEED_TIMELINE[i].created_at)).toBeGreaterThanOrEqual(Date.parse(FEED_TIMELINE[i - 1].created_at));
    }
    const through = FEED_TIMELINE.find((row) => row.id === FEED_SEEN.through);
    expect(through).toBeDefined();
    expect(Date.parse(through!.created_at)).toBeLessThanOrEqual(lookedAt);
    for (const row of FEED_TIMELINE) {
      const label = readFeedKind(row.kind)?.label ?? row.kind;
      expect(row.summary.trim().toLowerCase(), row.kind).not.toBe(label.toLowerCase());
    }
  });

  /*
    Exactly one, and it is the one the shots point at.

    One, because the roster has to photograph both answers at once: a project that has earned the
    third segment and three that have not. `10-project-state` and `11-project-code` shoot
    `/projects/alpha/...`, so the promotable row must be `alpha` and not any other — a state that
    has to be photographed on a project page can only live on the project that page opens.
  */
  it("has exactly one promotable project, and it is alpha", () => {
    const promotable = PROJECTS.filter((project) => project.promotable);
    expect(promotable.map((project) => project.project_id)).toEqual(["alpha"]);
  });

  /*
    And it is consistent with the numbers beside it, without the shell recomputing anything.

    `promotable` is the daemon's arithmetic and the shell only prints it — but a fixture that
    claimed promotable while its own columns said "3 of 5 classes still short of the bar" would
    photograph a row contradicting itself, which is a worse lie than a missing state. `shadow`
    because promotion is the crossing from shadow to active: a project already `active` has
    nothing left to earn, and an `off` one has recorded no evidence at all.
  */
  it("the promotable project is in shadow with every class clearing the bar", () => {
    const alpha = PROJECTS.find((project) => project.project_id === "alpha");
    expect(alpha).toBeDefined();
    expect(alpha?.mode).toBe("shadow");
    expect(alpha?.classes_ready).toBe(alpha?.classes_total);
    expect(alpha?.classes_total).toBeGreaterThan(0);
    // Restraint is the other half of the bar: at least one class the classifier withheld.
    expect(alpha?.withheld_classes_ready ?? 0).toBeGreaterThan(0);
  });

  /*
    The blocker keeps a row to be photographed in.

    Moving the 2-of-5 classes off alpha would otherwise have taken the "still short of the bar"
    sentence out of every shot — trading one unphotographed state for another.
  */
  it("still has a project short of the bar, so the blocker is photographed", () => {
    const short = PROJECTS.filter(
      (project) => project.classes_total > 0 && project.classes_ready < project.classes_total,
    );
    expect(short.length).toBeGreaterThan(0);
    expect(short.every((project) => !project.promotable)).toBe(true);
  });

  it("the judge is observing alpha, half way to its bar, with one of each verdict waiting", () => {
    const status = JUDGE_STATUS.alpha;
    expect(status.judge).toBe("observe");
    expect(status.readiness.ready).toBe(false);
    expect(status.readiness.reviewed).toBeLessThan(READINESS_MIN_REVIEWED);
    expect(status.readiness.by_class.length).toBeGreaterThanOrEqual(2);
    const verdicts = JUDGE_VERDICTS.alpha;
    expect(verdicts.filter((verdict) => verdict.band === "allow" && !verdict.capped)).toHaveLength(1);
    expect(verdicts.filter((verdict) => verdict.band === "allow" && verdict.capped)).toHaveLength(1);
    expect(verdicts.filter((verdict) => verdict.band === "deny")).toHaveLength(1);
  });

  it("the resolver is off for every project until somebody turns it on", () => {
    expect(judgeResolveFixture("nobody")).toEqual({
      project_id: "nobody",
      judge_resolve: "off",
      readiness: { reviewed: 0, agree: 0, less_cautious: 0, ready: false },
    });
  });

  it("the resolver is observing alpha, half way to its bar, with one block of each event waiting", () => {
    const status = JUDGE_RESOLVE_STATUS.alpha;
    expect(status.judge_resolve).toBe("observe");
    expect(status.readiness).toEqual({ reviewed: 6, agree: 6, less_cautious: 0, ready: false });
    expect(status.readiness.reviewed).toBeLessThan(RESOLVE_MIN_REVIEWED);
    expect(judgeResolveFixture("alpha")).toBe(status);
    const events = JUDGE_RESOLUTIONS.alpha.map((row) => row.event).sort();
    expect(events).toEqual(["gate_failed", "hard_deny", "park"]);
  });

  it("alpha's scoreboard agrees with its roster figures", () => {
    const alpha = PROJECTS.find((project) => project.project_id === "alpha");
    const alphaRows = SCOREBOARD.alpha;
    expect(alphaRows).toHaveLength(5);
    expect(alphaRows.every((row) => row.mode === "shadow")).toBe(true);
    expect(alphaRows.every((row) => row.reviewed >= READINESS_MIN_REVIEWED)).toBe(true);
    expect(
      alphaRows.every((row) => (row.agree / row.reviewed) * 100 >= READINESS_MIN_AGREE_PERCENT),
    ).toBe(true);
    expect(alphaRows.filter((row) => row.would_allow === 0).length).toBeGreaterThanOrEqual(2);
    expect(alphaRows).toHaveLength(alpha?.classes_total ?? 0);

    const deltaClearing = SCOREBOARD.delta.filter(
      (row) =>
        row.reviewed >= READINESS_MIN_REVIEWED &&
        (row.agree / row.reviewed) * 100 >= READINESS_MIN_AGREE_PERCENT,
    );
    expect(deltaClearing).toHaveLength(2);
  });

  /*
    The git queue answers something, and answers both kinds of row.

    `preview/daemon.ts` falls through to `[]` for any GET it has no branch for, so a missing route
    renders the empty state and the shot still looks correct. This route had no branch at all: the
    panel that lists every push, merge and rebase had never been seen holding anything.
  */
  it("serves a git queue holding rows that want a person and rows that do not", () => {
    expect(VCS_REQUESTS.length).toBeGreaterThan(0);
    const statuses = VCS_REQUESTS.map((row) => row.status);
    expect(statuses).toContain("escalated");
    expect(statuses).toContain("blocked");
    expect(statuses.some((status) => status !== "escalated" && status !== "blocked")).toBe(true);
    // Every row names a project, or `onlyProject()` drops it before the page ever draws it.
    expect(VCS_REQUESTS.every((row) => row.project_id !== "")).toBe(true);
  });

  it("the brain graph has a note-to-note link, a missing target and an archived note to reveal", () => {
    expect(NOTES_GRAPH.links.some((link) => link.target_kind === "note")).toBe(true);
    expect(NOTES_GRAPH.targets.some((target) => target.missing)).toBe(true);
    expect(OWNER_NOTES.some((note) => note.state === "archived")).toBe(true);
    expect(NOTES_GRAPH.notes.every((note) => note.state === "active")).toBe(true);
    // Every non-note link resolves to a target row, or the graph would draw an edge to nothing.
    for (const link of NOTES_GRAPH.links.filter((row) => row.target_kind !== "note")) {
      expect(NOTES_GRAPH.targets.some((target) => target.kind === link.target_kind && target.ref === link.target_ref), link.target_ref).toBe(true);
    }
  });
});

describe("the channel management fixtures", () => {
  const put = (body: unknown): RequestInit => ({ method: "PUT", body: JSON.stringify(body) });
  const CHANNEL_WHAT =
    "the Telegram channel's own settings: the doctrine it answers under and the project topics it posts in; the bot token is stored here too";

  it("the readout has telegram online and homed on a thread, and discord offline", () => {
    expect(CHANNELS.primary).toBe("telegram");
    expect(CHANNELS.routing.state).toBe("ok");
    const [telegram, discord] = CHANNELS.channels;
    expect(telegram.state).toBe("online");
    expect(telegram.home).toEqual({ conversation_ref: "-100123:5", kind: "thread", parent: "-100123", sender_ref: "4242" });
    expect(telegram.identities).toEqual({ senders: 1, conversations: 3 });
    expect(telegram.pairing).toBeNull();
    expect(telegram.outbox.failed).toBe(1);
    expect(telegram.drops.unknown_sender).toBe(3);
    expect(discord.state).toBe("offline");
    expect(discord.configured).toBe(true);
    expect(discord.capabilities?.buttons).toBe(false);
    expect(discord.capabilities?.threads).toBe(false);
    expect(Date.parse(discord.since as string)).toBe(NOW - 2 * 3_600_000);
    for (const channel of CHANNELS.channels) {
      expect(channel.health_row).toBe(`${channel.id}_sidecar`);
      expect(sidecarKeyOf(channel.health_row)).toBe(channel.id);
    }
  });

  it("serves the readout and the pairing routes", () => {
    expect(answer("/channels")).toBe(CHANNELS);
    expect(answer("/channels/telegram/pairing")).toMatchObject({ state: "waiting" });
    expect(answer("/channels/discord/pairing")).toMatchObject({ state: "paired" });
    expect(answer("/channels/zzz/pairing")).toEqual({ state: "none" });
    expect(PAIRING_STATUS.telegram.state).toBe("waiting");
    expect(answer("/channels/telegram/pairing", { method: "POST" })).toEqual(PAIRING_CODE);
    expect(PAIRING_CODE.code).toBe("482913");
    expect(PAIRING_CODE.command).toBe("/pair 482913");
    expect(Date.parse(PAIRING_CODE.expires_at)).toBe(NOW + 10 * 60_000);
    expect(answer("/channels/telegram/pairing", { method: "DELETE" })).toBeUndefined();
  });

  it("serves the identities, with one home and the group covering its threads", () => {
    const telegram = CHANNEL_IDENTITIES.telegram;
    expect(answer("/channels/telegram/identities")).toBe(telegram);
    const owner = telegram.identities.find((row) => row.sender_ref === "4242");
    expect(owner?.conversations).toHaveLength(3);
    expect(owner?.conversations.filter((row) => row.home)).toHaveLength(1);
    expect(owner?.conversations.find((row) => row.conversation_ref === "-100123")?.covers_threads).toBe(true);
    expect(telegram.projects).toEqual([{ project_id: "alpha", conversation_ref: "-100123:7" }]);
    expect(CHANNEL_IDENTITIES.discord.identities).toEqual([]);
    expect(answer("/channels/telegram/identities/4242", { method: "DELETE" })).toEqual({ removed: 1, home_cleared: false });
    expect(answer("/channels/telegram/identities/4242/home", put({ conversation_ref: "4242" }))).toEqual({ home: "4242", previous: "-100123:5" });
  });

  it("answers the primary switch, and clearing it never reseeds", () => {
    expect(answer("/channels/primary", put({ channel: "discord" }))).toEqual({ primary: "discord", reseeded: true });
    expect(answer("/channels/primary", put({ channel: "telegram" }))).toEqual({ primary: "telegram", reseeded: false });
    expect(answer("/channels/primary", put({ channel: null }))).toEqual({ primary: null, reseeded: false });
  });

  it("keeps the old policy shape and serves the per-channel one", () => {
    const old = answer("/notifications/policy") as { families: unknown; kinds: unknown };
    expect(Object.keys(old).sort()).toEqual(["families", "kinds"]);
    for (const id of ["telegram", "discord"]) {
      const p = answer(`/notifications/policy?channel=${id}`) as (typeof CHANNEL_NOTIFY_POLICY)[string];
      expect(p).toBe(CHANNEL_NOTIFY_POLICY[id]);
      expect(p.channel).toBe(id);
      expect(mergePolicy(p.default, p.overrides)).toEqual(p.resolved);
    }
    expect(CHANNEL_NOTIFY_POLICY.telegram.default).toEqual(old);
    expect(CHANNEL_NOTIFY_POLICY.telegram.primary).toBe(true);
    expect(CHANNEL_NOTIFY_POLICY.discord.primary).toBe(false);
    expect(CHANNEL_NOTIFY_POLICY.discord.overrides).toEqual({ families: [], kinds: [] });
    expect(answer("/notifications/policy?channel=telegram", put({ families: [], kinds: [] }))).toBeUndefined();
  });

  it("aligns the older fixtures with the channels", () => {
    expect(OWNER_NOTES.find((note) => note.id === 2)?.origin).toBe("channel:telegram");
    const config = answer("/config/machine") as MachineConfig;
    const area = config.settings.find((row) => row.area === "channels/telegram");
    expect(area?.path).toBe("channels/telegram.yaml");
    expect(area?.what).toBe(CHANNEL_WHAT);
    expect(config.settings.some((row) => row.area === "telegram")).toBe(true);
    const secrets = answer("/config/secrets") as { secrets: { key: string; area: string }[] };
    expect(secrets.secrets.find((row) => row.key === "telegram-token")?.area).toBe("channels/telegram");
    const health = answer("/health/readout") as { subsystems: { name: string; status: string; reason?: string }[] };
    expect(health.subsystems.find((row) => row.name === "telegram_sidecar")).toEqual({ name: "telegram_sidecar", status: "ok" });
  });
});
