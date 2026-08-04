import { describe, expect, it } from "vitest";

import type { Budget, ClassTally, JobItem, ProjectSummary } from "./api";
import {
  agreementRate,
  autopilotState,
  base64ToBytes,
  breadcrumbs,
  budgetStatusLabel,
  classifierVerdictLabel,
  concatSamples,

  feedKindLabel,
  formatBytes,
  CONTEXT_WINDOW_TOKENS,
  contextPressure,
  formatTokens,
  formatUsd,
  gateTone,
  groupScoreboardByMode,
  healthReasonLabel,
  healthTone,
  joinPath,

  jobEndingLabel,
  jobIsLive,
  jobItemLabel,
  jobItemTone,
  jobProgress,
  jobStageLabel,
  killSwitchLabel,
  mailLabel,
  mailTone,
  parentPath,
  periodLabel,
  quotedReply,
  replySubject,
  requeueFailureMessage,
  runIsLive,
  sendFailureMessage,
  runStatusLabel,
  runTone,
  spokenDuration,
  tokenLevelHint,
  voiceCleanupLabel,
  voiceCleanupTone,
  promotionBlock,
  promotionCriterionGap,
  promotionReadiness,
  queueBlock,
  readinessCriterionLabel,
  readinessGap,
  relativeTime,
  REVIEW_ALLOW,
  REVIEW_BLOCK,
  safeDownloadName,
  scoreboardReadiness,
  totalPending,
} from "./derive";

describe("pure UI derivations", () => {
  it("sums pending work across projects, including an empty list", () => {
    const cases = [
      { projects: [] satisfies ProjectSummary[], expected: 0 },
      {
        projects: [
          { project_id: "alpha", mode: "off", project_root: null, pending: 2, classes_ready: 0, classes_total: 0, promotable: false, open_proposals: 0, wip_limit: 3, queue_full: false },
          { project_id: "beta", mode: "shadow", project_root: null, pending: 0, classes_ready: 0, classes_total: 0, promotable: false, open_proposals: 0, wip_limit: 3, queue_full: false },
          { project_id: "gamma", mode: "active", project_root: null, pending: 5, classes_ready: 0, classes_total: 0, promotable: false, open_proposals: 0, wip_limit: 3, queue_full: false },
        ] satisfies ProjectSummary[],
        expected: 7,
      },
    ];

    for (const { projects, expected } of cases) {
      expect(totalPending(projects)).toBe(expected);
    }
  });

  it("locks promotion until the scoreboard earns it, and never gates an active project", () => {
    const project = (
      over: Partial<ProjectSummary>,
    ): ProjectSummary => ({
      project_id: "alpha",
      mode: "shadow",
      project_root: null,
      pending: 0,
      classes_ready: 0,
      classes_total: 0,
      promotable: false,
      open_proposals: 0,
      wip_limit: 3,
      queue_full: false,
      ...over,
    });

    // No evidence yet reads differently from "tried and fell short".
    expect(promotionBlock(project({}))).toBe("No reviewed shadow decisions yet");
    expect(promotionBlock(project({ classes_ready: 1, classes_total: 3 }))).toBe(
      "1/3 action classes ready",
    );
    // The daemon owns the verdict — when it says promotable, nothing is blocked.
    expect(
      promotionBlock(
        project({ classes_ready: 3, classes_total: 3, promotable: true }),
      ),
    ).toBeNull();
    // The gate guards the way IN to autonomy; an already-active project is never re-gated.
    expect(promotionBlock(project({ mode: "active" }))).toBeNull();
  });

  it("explains a project gone quiet on a full approval queue", () => {
    const project = (over: Partial<ProjectSummary>): ProjectSummary => ({
      project_id: "alpha",
      mode: "active",
      project_root: null,
      pending: 0,
      classes_ready: 0,
      classes_total: 0,
      promotable: false,
      open_proposals: 0,
      wip_limit: 3,
      queue_full: false,
      ...over,
    });

    // Room to spare says nothing — the note only appears when it explains something.
    expect(queueBlock(project({ open_proposals: 2 }))).toBeNull();
    // The daemon decides fullness; the copy points at reviewing, since that is what releases it.
    expect(queueBlock(project({ open_proposals: 3, queue_full: true }))).toBe(
      "3/3 proposals waiting — new work is deferred until you review one",
    );
    // A limit the daemon reports as null is a ceiling nobody set; printing it
    // as "3/0" would state an impossibility where the count alone is honest.
    expect(
      queueBlock(project({ open_proposals: 3, queue_full: true, wip_limit: null })),
    ).toBe("3 proposals waiting — new work is deferred until you review one");
  });

  it("derives agreement rates, including the zero-reviewed edge case", () => {
    const cases = [
      {
        tally: {
          mode: "shadow",
          action_class: "filesystem.write",
          total: 0,
          would_allow: 0,
          would_pend: 0,
          would_deny: 0,
          reviewed: 0,
          agree: 0,
          disagree: 0,
        } satisfies ClassTally,
        expected: null,
      },
      {
        tally: {
          mode: "active",
          action_class: "shell.execute",
          total: 4,
          would_allow: 3,
          would_pend: 1,
          would_deny: 0,
          reviewed: 4,
          agree: 3,
          disagree: 1,
        } satisfies ClassTally,
        expected: 0.75,
      },
    ];

    for (const { tally, expected } of cases) {
      expect(agreementRate(tally)).toBe(expected);
    }
  });

  it("groups an empty scoreboard into an empty record", () => {
    expect(groupScoreboardByMode([] satisfies ClassTally[])).toEqual({});
  });

  it("groups scoreboard tallies by mode while preserving group order", () => {
    const tallies = [
      {
        mode: "shadow",
        action_class: "filesystem.write",
        total: 3,
        would_allow: 1,
        would_pend: 2,
        would_deny: 0,
        reviewed: 2,
        agree: 2,
        disagree: 0,
      },
      {
        mode: "active",
        action_class: "shell.execute",
        total: 2,
        would_allow: 2,
        would_pend: 0,
        would_deny: 0,
        reviewed: 1,
        agree: 1,
        disagree: 0,
      },
      {
        mode: "shadow",
        action_class: "network.request",
        total: 4,
        would_allow: 2,
        would_pend: 1,
        would_deny: 1,
        reviewed: 3,
        agree: 2,
        disagree: 1,
      },
    ] satisfies ClassTally[];

    expect(groupScoreboardByMode(tallies)).toEqual({
      shadow: [tallies[0], tallies[2]],
      active: [tallies[1]],
    });
  });

  it("derives the kill-switch labels", () => {
    const cases = [
      [true, "Kill switch engaged — autopilot paused"],
      [false, "Kill switch off"],
    ] as const;

    for (const [engaged, expected] of cases) {
      expect(killSwitchLabel(engaged)).toBe(expected);
    }
  });

  it("formats USD amounts to two decimals", () => {
    expect(formatUsd(1.5)).toBe("$1.50");
    expect(formatUsd(0)).toBe("$0.00");
    expect(formatUsd(12.345)).toBe("$12.35");
  });

  it("labels budget periods", () => {
    expect(periodLabel("daily")).toBe("today");
    expect(periodLabel("weekly")).toBe("this week");
    expect(periodLabel("monthly")).toBe("this month");
  });

  it("derives budget status labels for unset, active, and paused budgets", () => {
    const base = {
      limit_usd: null,
      period: "monthly",
      hourly_limit_usd: null,
      per_run_reserve_usd: 0.5,
      time_cost_per_hour_usd: 3,
      window_spend_usd: 0,
      hourly_spend_usd: 0,
      paused: false,
      reason: null,
    } satisfies Budget;

    expect(budgetStatusLabel(base)).toBe("No spending limit set");
    expect(
      budgetStatusLabel({ ...base, limit_usd: 10, window_spend_usd: 1.5 }),
    ).toBe("$1.50 of $10.00 this month");
    expect(
      budgetStatusLabel({ ...base, limit_usd: 10, window_spend_usd: 12, paused: true }),
    ).toBe("Paused — $12.00 of $10.00 this month");
  });

  it("derives promotion readiness per action class", () => {
    const base = {
      mode: "shadow",
      action_class: "filesystem.write",
      total: 0,
      would_allow: 0,
      would_pend: 0,
      would_deny: 0,
      reviewed: 0,
      agree: 0,
      disagree: 0,
    } satisfies ClassTally;

    expect(promotionReadiness(base)).toEqual({ ready: false, rate: null, samples: 0 });
    expect(
      promotionReadiness({ ...base, reviewed: 10, agree: 10 }),
    ).toEqual({ ready: true, rate: 1, samples: 10 });
    expect(
      promotionReadiness({ ...base, reviewed: 10, agree: 9 }),
    ).toEqual({ ready: false, rate: 0.9, samples: 10 });
    expect(
      promotionReadiness({ ...base, reviewed: 5, agree: 5 }),
    ).toEqual({ ready: false, rate: 1, samples: 5 });
  });

  it("explains the gap to readiness, or null when ready", () => {
    const base = {
      mode: "shadow",
      action_class: "filesystem.write",
      total: 0,
      would_allow: 0,
      would_pend: 0,
      would_deny: 0,
      reviewed: 0,
      agree: 0,
      disagree: 0,
    } satisfies ClassTally;

    expect(readinessGap({ ...base, reviewed: 10, agree: 10 })).toBeNull();
    expect(readinessGap(base)).toBe("10 more reviews");
    expect(readinessGap({ ...base, reviewed: 4, agree: 4 })).toBe("6 more reviews");
    expect(readinessGap({ ...base, reviewed: 9, agree: 9 })).toBe("1 more review");
    expect(readinessGap({ ...base, reviewed: 10, agree: 9 })).toBe("90% agreement");
    expect(readinessGap({ ...base, reviewed: 20, agree: 18 })).toBe("90% agreement");
  });

  it("counts ready classes across a scoreboard group", () => {
    const base = {
      mode: "shadow",
      action_class: "a",
      total: 0,
      would_allow: 0,
      would_pend: 0,
      would_deny: 0,
      reviewed: 0,
      agree: 0,
      disagree: 0,
    } satisfies ClassTally;

    expect(scoreboardReadiness([])).toEqual({ ready: 0, total: 0 });
    expect(
      scoreboardReadiness([
        { ...base, action_class: "a", reviewed: 10, agree: 10 },
        { ...base, action_class: "b", reviewed: 3, agree: 3 },
      ]),
    ).toEqual({ ready: 1, total: 2 });
  });

  it("states the promotion criterion", () => {
    expect(readinessCriterionLabel()).toBe("Ready at 10+ reviews, ≥95% agreement");
  });

  it("names_the_unmet_promotion_criterion", () => {
    expect(
      promotionCriterionGap({
        classes_ready: 2,
        classes_total: 2,
        withheld_classes_ready: 0,
      }),
    ).toBe("No withheld action class has cleared the review bar");
    expect(
      promotionCriterionGap({
        classes_ready: 1,
        classes_total: 2,
        withheld_classes_ready: 0,
      }),
    ).toBe("1/2 action classes ready");
  });

  it("derives the headline autopilot state in priority order", () => {
    const calm = {
      killEngaged: false,
      budgetPaused: false,
      isFirstProject: false,
      proposalCount: 0,
      pending: 0,
    };

    expect(autopilotState(calm)).toBe("quiet");
    expect(autopilotState({ ...calm, pending: 2 })).toBe("pending");
    expect(autopilotState({ ...calm, proposalCount: 4 })).toBe("swamped");
    // The queue is only "swamped" strictly beyond the threshold.
    expect(autopilotState({ ...calm, proposalCount: 3 })).toBe("quiet");
    expect(autopilotState({ ...calm, isFirstProject: true })).toBe("first");
    expect(autopilotState({ ...calm, budgetPaused: true })).toBe("budget");
    expect(autopilotState({ ...calm, killEngaged: true })).toBe("kill");
    // A null kill switch (not yet loaded) counts as not engaged.
    expect(autopilotState({ ...calm, killEngaged: null, pending: 1 })).toBe("pending");

    // Priority: each higher state wins over every lower one at once.
    expect(
      autopilotState({
        killEngaged: true,
        budgetPaused: true,
        isFirstProject: true,
        proposalCount: 9,
        pending: 9,
      }),
    ).toBe("kill");
    expect(
      autopilotState({
        ...calm,
        budgetPaused: true,
        isFirstProject: true,
        proposalCount: 9,
        pending: 9,
      }),
    ).toBe("budget");
    expect(autopilotState({ ...calm, proposalCount: 4, pending: 9 })).toBe("swamped");
  });

  it("formats human relative times, with fallbacks", () => {
    const now = Date.parse("2026-07-26T12:00:00Z");
    expect(relativeTime("2026-07-26T12:00:00Z", now)).toBe("just now");
    expect(relativeTime("2026-07-26T11:59:30Z", now)).toBe("just now");
    expect(relativeTime("2026-07-26T12:00:30Z", now)).toBe("just now");
    expect(relativeTime("2026-07-26T11:48:00Z", now)).toBe("12 min ago");
    expect(relativeTime("2026-07-26T09:00:00Z", now)).toBe("3 h ago");
    expect(relativeTime("2026-07-24T12:00:00Z", now)).toBe("2 d ago");
    expect(relativeTime("2026-07-12T12:00:00Z", now)).toBe("2 w ago");
    expect(relativeTime("2026-05-01T00:00:00Z", now)).toBe("2026-05-01");
    expect(relativeTime("not-a-date", now)).toBe("not-a-date");
  });

  it("maps triage classes onto the app's state vocabulary", () => {
    expect(mailTone("urgent")).toBe("pending");
    expect(mailTone("action")).toBe("paused");
    // `failed` is not a verdict about the content — it is mail that still wants a requeue, so it
    // keeps `action`'s tone rather than receding with `noise`.
    expect(mailTone("failed")).toBe("paused");
    expect(mailTone("info")).toBe("active");
    expect(mailTone("noise")).toBe("off");
    // No verdict yet is a state of its own, not a missing one.
    expect(mailTone(null)).toBe("shadow");
    // A class the núcleo invents later must recede, never shout.
    expect(mailTone("whatever-comes-next")).toBe("off");
  });

  it("labels an unjudged message as waiting rather than blank", () => {
    expect(mailLabel(null)).toBe("waiting");
    expect(mailLabel("urgent")).toBe("urgent");
  });

  it("formats attachment sizes the way a file manager does", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(999)).toBe("999 B");
    // Decimal units, matching the OS the file lands on rather than the binary convention.
    expect(formatBytes(1000)).toBe("1.0 kB");
    expect(formatBytes(847300)).toBe("847 kB");
    expect(formatBytes(1_400_000)).toBe("1.4 MB");
    expect(formatBytes(2_500_000_000)).toBe("2.5 GB");
    // Larger than the last unit keeps counting in GB rather than inventing one.
    expect(formatBytes(9_000_000_000_000)).toBe("9000 GB");
    expect(formatBytes(-1)).toBe("—");
  });

  it("decodes base64 attachment content to real bytes", () => {
    // "ola" is ASCII and would survive almost any mistake; the accented and high bytes are the
    // point — writing atob's char codes straight into a Blob re-encodes them as UTF-8 and quietly
    // corrupts every file that is not plain ASCII.
    expect(Array.from(base64ToBytes("b2xh"))).toEqual([111, 108, 97]);
    expect(Array.from(base64ToBytes("w6k="))).toEqual([0xc3, 0xa9]);
    // The first bytes of a .docx (a zip): 0x50 0x4b 0x03 0x04.
    expect(Array.from(base64ToBytes("UEsDBA=="))).toEqual([0x50, 0x4b, 0x03, 0x04]);
    expect(base64ToBytes("").length).toBe(0);
  });

  it("makes a sender's filename safe to save under", () => {
    // Downloading through a blob is the only way to send the bearer token, and it skips the
    // Content-Disposition the daemon sanitised — so the name is made safe again here.
    expect(safeDownloadName("../../.ssh/authorized_keys")).toBe("authorized_keys");
    expect(safeDownloadName("..\\..\\System32\\evil.dll")).toBe("evil.dll");
    expect(safeDownloadName("rela\u0000torio.docx")).toBe("relatorio.docx");
    expect(safeDownloadName(null)).toBe("attachment.bin");
    expect(safeDownloadName("")).toBe("attachment.bin");
    expect(safeDownloadName("..")).toBe("attachment.bin");
    expect(safeDownloadName("MÉDIAS_ESPERADAS.docx")).toBe("MÉDIAS_ESPERADAS.docx");
  });
});

describe("shadow review asks the question the gate actually scores", () => {
  /**
   * The daemon's `AGREE_CASE` (core/src/shadow.rs), mirrored here ONLY to prove the buttons
   * answer the same question it asks: `approve` agrees with `allow`, and `reject` agrees with
   * both `deny` and `pending_approval`.
   */
  function daemonScoresAsAgreement(verdict: string, classifierDecision: string): boolean {
    if (verdict === "approve") return classifierDecision === "allow";
    if (verdict === "reject") return classifierDecision === "deny" || classifierDecision === "pending_approval";
    return false;
  }

  const CLASSIFIER_VERDICTS = ["allow", "deny", "pending_approval"];

  it("scores agreement whenever the reviewer would do what the classifier proposed", () => {
    const wouldDoTheSame = (decision: string) =>
      decision === "allow" ? REVIEW_ALLOW.verdict : REVIEW_BLOCK.verdict;

    for (const decision of CLASSIFIER_VERDICTS) {
      expect(daemonScoresAsAgreement(wouldDoTheSame(decision), decision)).toBe(true);
    }
  });

  it("scores disagreement whenever the reviewer would do the opposite", () => {
    const wouldDoTheOpposite = (decision: string) =>
      decision === "allow" ? REVIEW_BLOCK.verdict : REVIEW_ALLOW.verdict;

    for (const decision of CLASSIFIER_VERDICTS) {
      expect(daemonScoresAsAgreement(wouldDoTheOpposite(decision), decision)).toBe(false);
    }
  });

  it("labels each button by the action it takes, never by agreement", () => {
    // Pinning the literal copy on purpose. The labels ARE the semantics here: they used to read
    // "Agree"/"Disagree", which asks about the CLASSIFICATION rather than the ACTION. The two
    // questions are inverse on every row the classifier did not `allow`, so agreeing that an
    // action should require approval sent `approve` — scored above as a disagreement. The gate
    // then punished the reviewer who read and rewarded the one who waved everything through.
    expect(REVIEW_ALLOW.label).toBe("Allow");
    expect(REVIEW_BLOCK.label).toBe("Block");
  });

  it("phrases the classifier's own verdict as an answer to that same question", () => {
    expect(classifierVerdictLabel("allow")).toBe("would allow");
    expect(classifierVerdictLabel("deny")).toBe("would block");
    expect(classifierVerdictLabel("pending_approval")).toBe("would ask you");
    // A verdict the daemon gains later still renders, rather than vanishing from the row.
    expect(classifierVerdictLabel("quarantine")).toBe("would quarantine");
  });
});

describe("run, health and key derivations", () => {
  it("tones a run by whether it wants attention, not by whether it succeeded", () => {
    expect(runTone("completed")).toBe("active");
    expect(runTone("running")).toBe("shadow");
    expect(runTone("pending")).toBe("shadow");
    // Waiting on a signature is the one state that should be loud.
    expect(runTone("awaiting_approval")).toBe("pending");
    expect(runTone("failed")).toBe("paused");
    expect(runTone("timed_out")).toBe("paused");
    // Already over and nobody is waiting: it recedes.
    expect(runTone("cancelled")).toBe("off");
    expect(runTone("interrupted")).toBe("off");
    expect(runTone("something_new")).toBe("off");
  });

  it("reads the two underscored statuses as words", () => {
    expect(runStatusLabel("awaiting_approval")).toBe("awaiting approval");
    expect(runStatusLabel("timed_out")).toBe("timed out");
    expect(runStatusLabel("completed")).toBe("completed");
  });

  it("counts only pending and running as live, so polling always terminates", () => {
    expect(runIsLive("pending")).toBe(true);
    expect(runIsLive("running")).toBe(true);
    expect(runIsLive("completed")).toBe(false);
    expect(runIsLive("awaiting_approval")).toBe(false);
    // An unrecognised status counts as settled: polling forever is the worse mistake.
    expect(runIsLive("something_new")).toBe(false);
  });

  it("has no gate tone for a run that never reached the gate", () => {
    expect(gateTone(null)).toBeNull();
    expect(gateTone("passed")).toBe("active");
    expect(gateTone("failed")).toBe("paused");
  });

  it("makes down louder than degraded, and lets disabled recede", () => {
    expect(healthTone("ok")).toBe("active");
    expect(healthTone("degraded")).toBe("paused");
    expect(healthTone("down")).toBe("pending");
    // A subsystem nobody turned on is not a fault — health.rs keeps it out of the aggregate too.
    expect(healthTone("disabled")).toBe("off");
    expect(healthTone("unknown-to-this-shell")).toBe("off");
  });

  it("spells out a diagnostic slug, and shows an unknown one rather than hiding it", () => {
    expect(healthReasonLabel(undefined)).toBeNull();
    expect(healthReasonLabel("not-configured")).toBe("not configured");
    expect(healthReasonLabel("low-disk-space")).toBe("low disk space");
    expect(healthReasonLabel("something-new")).toBe("something-new");
  });

  it("says what each key level buys its holder", () => {
    expect(tokenLevelHint("read-only")).toContain("Cannot start a run");
    expect(tokenLevelHint("run-creating")).toContain("Cannot mint");
    expect(tokenLevelHint("admin")).toContain("Full access");
  });

  it("explains a requeue refusal as the thing to do about it", () => {
    expect(requeueFailureMessage("unknown")).toContain("no longer in the mailbox");
    expect(requeueFailureMessage("conflict")).toContain("retention");
    expect(requeueFailureMessage("failed")).toContain("refused");
  });

  it("prefixes a reply subject once, however many hops it has already made", () => {
    expect(replySubject("the roof")).toBe("Re: the roof");
    // The case this exists for: a thread that already carries the prefix must not collect another.
    expect(replySubject("Re: the roof")).toBe("Re: the roof");
    expect(replySubject("RE: the roof")).toBe("RE: the roof");
    expect(replySubject("re: the roof")).toBe("re: the roof");
    // Surrounding whitespace is the sender's, not a difference in subject.
    expect(replySubject("  the roof  ")).toBe("Re: the roof");
    expect(replySubject("  Re: the roof")).toBe("Re: the roof");
    // No subject is a state, and "Re:" says more about what this is than an empty line does.
    expect(replySubject(null)).toBe("Re:");
    expect(replySubject("")).toBe("Re:");
    expect(replySubject("   ")).toBe("Re:");
  });

  it("quotes the message being answered, and quotes nothing when there is nothing left", () => {
    expect(quotedReply("Maria", "first\nsecond")).toBe(
      "\n\nMaria wrote:\n> first\n> second\n",
    );
    // A blank line inside the quote stays a blank quoted line, not a line with a trailing space.
    expect(quotedReply("Maria", "first\n\nsecond")).toBe(
      "\n\nMaria wrote:\n> first\n>\n> second\n",
    );
    // CRLF arrives from real mailboxes and must not leave a stray \r inside a quoted line.
    expect(quotedReply("Maria", "first\r\nsecond")).toBe(
      "\n\nMaria wrote:\n> first\n> second\n",
    );
    // Retention pruned the body: quoting an empty block would assert the sender wrote nothing.
    expect(quotedReply("Maria", null)).toBe("");
    expect(quotedReply("Maria", "")).toBe("");
    expect(quotedReply("Maria", "  \n  ")).toBe("");
  });

  it("says whether a message that did not go was ever attempted", () => {
    // The three that never left this process say so, because that is a fact and it is the one the
    // person needs before deciding whether to press send again.
    expect(sendFailureMessage({ kind: "invalid", reason: "a recipient must not contain a line break" }))
      .toContain("Not sent");
    expect(sendFailureMessage({ kind: "unconfigured", reason: "no submission host is configured" }))
      .toContain("not attempted");
    expect(sendFailureMessage({ kind: "unreachable", reason: "the daemon is not reachable" }))
      .toContain("not attempted");
    // The one that did leave must NOT claim it did not, and must say where to look.
    const undelivered = sendFailureMessage({ kind: "undelivered", reason: "the email sidecar could not send the message" });
    expect(undelivered).not.toContain("not attempted");
    expect(undelivered).toContain("sent mailbox");
    // Each keeps the daemon's own sentence, which is the half naming the field or the file.
    expect(sendFailureMessage({ kind: "unconfigured", reason: "set smtp_host in .ai/email.yaml" }))
      .toContain("smtp_host");
  });

  it("builds a breadcrumb trail that always starts at the root", () => {
    expect(breadcrumbs("")).toEqual([{ label: "/", path: "" }]);
    expect(breadcrumbs("core/src/http.rs")).toEqual([
      { label: "/", path: "" },
      { label: "core", path: "core" },
      { label: "src", path: "core/src" },
      { label: "http.rs", path: "core/src/http.rs" },
    ]);
    // Stray separators do not become empty crumbs that navigate nowhere.
    expect(breadcrumbs("/core//src/")).toEqual([
      { label: "/", path: "" },
      { label: "core", path: "core" },
      { label: "src", path: "core/src" },
    ]);
  });

  it("joins onto the root without a leading slash, which the daemon reads as absolute", () => {
    expect(joinPath("", "core")).toBe("core");
    expect(joinPath("core/src", "http.rs")).toBe("core/src/http.rs");
  });

  it("walks up to the root and stays there", () => {
    expect(parentPath("core/src/http.rs")).toBe("core/src");
    expect(parentPath("core")).toBe("");
    expect(parentPath("")).toBe("");
  });

  it("abbreviates token counts, and shows a missing one as a dash", () => {
    expect(formatTokens(null)).toBe("—");
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(1500)).toBe("1.5k");
    expect(formatTokens(48000)).toBe("48k");
    expect(formatTokens(1_400_000)).toBe("1.4M");
  });
});

describe("concatSamples", () => {
  it("joins the callback's chunks in order", () => {
    const joined = concatSamples([
      new Float32Array([0.1, 0.2]),
      new Float32Array([0.3]),
      new Float32Array([0.4, 0.5]),
    ]);
    expect(Array.from(joined).map((n) => Math.round(n * 10) / 10)).toEqual([0.1, 0.2, 0.3, 0.4, 0.5]);
  });

  it("handles a recording that produced nothing", () => {
    // A microphone that opened and closed before any callback fired. The caller checks the length
    // rather than posting an empty WAV to the daemon.
    expect(concatSamples([]).length).toBe(0);
    expect(concatSamples([new Float32Array(0)]).length).toBe(0);
  });
});

describe("voice", () => {
  it("does not paint an uncleaned transcript as a failure", () => {
    // Both `raw` and `shrunk` still carry a complete transcript -- the daemon refuses a cleanup
    // rather than letting it damage what was said -- so neither may read as something broken.
    expect(voiceCleanupTone("cleaned")).toBe("active");
    expect(voiceCleanupTone("shrunk")).toBe("paused");
    expect(voiceCleanupTone("raw")).toBe("off");
  });

  it("falls back to the state's own name rather than inventing a tone", () => {
    // A shell can be newer or older than the daemon it talks to; an unknown state must recede, not
    // claim anything.
    expect(voiceCleanupTone("something-new")).toBe("off");
    expect(voiceCleanupLabel("something-new")).toBe("something-new");
  });

  it("says what the three states mean in words", () => {
    expect(voiceCleanupLabel("cleaned")).toBe("cleaned");
    expect(voiceCleanupLabel("raw")).toBe("as spoken");
    expect(voiceCleanupLabel("shrunk")).toBe("cleanup refused");
  });

  it("reads a spoken length as minutes and seconds", () => {
    expect(spokenDuration(0)).toBe("0s");
    expect(spokenDuration(4200)).toBe("4s");
    expect(spokenDuration(59_400)).toBe("59s");
    expect(spokenDuration(60_000)).toBe("1m 00s");
    // Twenty minutes is the cap a capture may not exceed.
    expect(spokenDuration(1_200_000)).toBe("20m 00s");
    expect(spokenDuration(-1)).toBe("—");
    expect(spokenDuration(Number.NaN)).toBe("—");
  });
});

describe("jobs", () => {
  const item = (over: Partial<JobItem> = {}): JobItem => ({
    ordinal: 0,
    description: "an item",
    status: "pending",
    run_id: null,
    gate_status: null,
    ...over,
  });

  it("never lets a gate that could not run read as a gate that failed", () => {
    // The pair the daemon keeps apart from gate.rs all the way up to jobs.status. A non-zero exit
    // says the code is broken; a command that would not start says nothing was ever measured.
    // Collapsing them here, at the last step, wastes every one of those and tells somebody their
    // tests failed when no test ever ran.
    const red = jobEndingLabel("gate_failed");
    const unmeasured = jobEndingLabel("gate_errored");
    expect(red).not.toBe(unmeasured);
    expect(unmeasured).toContain("nothing was measured");
    expect(red).toContain("red");
  });

  it("keeps running out of time apart from running out of money", () => {
    // One means the clock beat it and the rest of the list is still worth doing; the other means
    // starting again today stops in the same place. Same-looking rows, opposite next moves.
    expect(jobEndingLabel("expired")).toContain("time");
    expect(jobEndingLabel("stopped")).toContain("budget");
  });

  it("shows an ending it has never heard of rather than swallowing it", () => {
    // A shell can be older than the daemon it talks to. An unrecognised ending is still an ending,
    // and hiding it would leave the row looking unfinished forever.
    expect(jobEndingLabel("quarantined")).toBe("quarantined");
  });

  it("says which kind of waiting a waiting job is doing", () => {
    // `waiting` covers a budget window that will reopen and a slot another run holds, and those
    // ask opposite things of the reader. A bare "waiting" makes them guess.
    expect(jobStageLabel("waiting", "budget")).toContain("budget");
    expect(jobStageLabel("waiting", "slot")).toContain("something else");
    expect(jobStageLabel("waiting", "attention")).toContain("keyboard");
    // A reason from a newer daemon still renders as waiting rather than as an ending.
    expect(jobStageLabel("waiting", "moon-phase")).toBe("waiting to continue");
  });

  it("hands a finished job's row to the ending vocabulary", () => {
    expect(jobStageLabel("gate_errored", null)).toBe(jobEndingLabel("gate_errored"));
    expect(jobStageLabel("planning", null)).toContain("what to do");
  });

  it("counts only items that were actually finished", () => {
    // `implemented` means the node finished and the gate has not measured it yet — which is the
    // whole reason the gate runs between items. Counting it would let the bar reach the end with a
    // red gate still to come.
    const progress = jobProgress([
      item({ status: "passed" }),
      item({ status: "implemented" }),
      item({ status: "pending" }),
    ]);
    expect(progress).toEqual({ done: 1, total: 3 });
  });

  it("does not claim a verdict for an item nothing measured", () => {
    // Legitimate: the project configures no gate command, or gate_after_each_item is off and this
    // was not the last item. A flat "passed" for both would claim a verdict nobody produced.
    expect(jobItemLabel(item({ status: "passed", gate_status: "passed" }))).toContain("tests");
    expect(jobItemLabel(item({ status: "passed" }))).toBe("done, not measured");
  });

  it("tells a stopped item from an unfinished one in its tone", () => {
    expect(jobItemTone("passed")).toBe("active");
    expect(jobItemTone("running")).toBe("pending");
    expect(jobItemTone("gate_errored")).toBe("paused");
    expect(jobItemTone("pending")).toBe("off");
    expect(jobItemTone("something-new")).toBe("off");
  });

  it("knows which statuses still hold the project", () => {
    expect(jobIsLive("waiting")).toBe(true);
    expect(jobIsLive("implementing")).toBe(true);
    expect(jobIsLive("cancelled")).toBe(false);
    expect(jobIsLive("completed")).toBe(false);
  });

  it("translates job feed kinds and leaves every other kind alone", () => {
    expect(feedKindLabel("job_gate_failed")).toBe("job gate");
    expect(feedKindLabel("job_expired")).toBe("job ran out of time");
    // The feed prints kinds verbatim for everything else, and a daemon that starts emitting a new
    // one must still show it rather than showing nothing.
    expect(feedKindLabel("run_retry")).toBe("run_retry");
  });
});

describe("context pressure", () => {
  it("says nothing at all when a run has not reported its context", () => {
    // Not zero. A run that has said nothing about its context and one that has said "nearly empty"
    // are different facts, and an empty bar would state the first as if it were the second.
    expect(contextPressure(null)).toBeNull();
  });

  it("reads the fill against the window the daemon hands off at", () => {
    const half = contextPressure(CONTEXT_WINDOW_TOKENS / 2);
    expect(half).toEqual({ fill: 100_000, fraction: 0.5, handingOff: false });
  });

  it("flags the handoff line exactly at four fifths, not past it", () => {
    // The daemon's own comparison is `fill * 5 >= limit * 4`, so the boundary itself counts as
    // crossed. Drawing it as "not yet" would show a run as safe on the very tick it splits.
    expect(contextPressure(160_000)?.handingOff).toBe(true);
    expect(contextPressure(159_999)?.handingOff).toBe(false);
  });

  it("never reports more than a full bar", () => {
    // The window is a conservative floor, so a model with a bigger one genuinely reports past it.
    // That is a real number and worth showing — a bar wider than its own track is not.
    const over = contextPressure(CONTEXT_WINDOW_TOKENS * 3);
    expect(over?.fraction).toBe(1);
    expect(over?.fill).toBe(600_000);
  });

  it("ignores a fill that cannot be one", () => {
    expect(contextPressure(-1)).toBeNull();
    expect(contextPressure(Number.NaN)).toBeNull();
  });
});
