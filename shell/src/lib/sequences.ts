import { readFeedKind, waitReasonFromSummary, type FeedEntry } from "../data/feed";
import { FEED_LANES, feedGravityOf, feedKindLeavesOpen, feedLaneOf, feedMarkTone, type FeedGravity, type FeedLane } from "../ui/lanes";
import { readState } from "../ui/state-map";

/**
 * The Feed's trace, pure: which lines belong together, what each group is called, how it ended,
 * and where it stood at any moment of the replay.
 *
 * A line is a moment; what a person asks about is a THING — did job 57 land, is run 900612 still
 * trying. So the trace draws sequences: every line about one subject is one row, a bar from its
 * first line to its last. The subject is the núcleo's (`job:57`, `run:900598`), with the older
 * owners as the fallback for a line written before subjects existed — a run id, then an errand.
 *
 * A line with none of them is about nothing that lasts — a digest, a config written, an urgent
 * e-mail — and those group by KIND into a series: eight nightly digests are one row of eight
 * marks, not eight rows. A series has no bar and is never open, because nothing ran between its
 * lines; a series of one is simply that line.
 *
 * Lane, gravity, tone and the open kinds are claims about núcleo kinds and are read from
 * `ui/lanes.ts`, never decided here; this module only says what a group of lines adds up to.
 */

/**
 * The subject families the núcleo writes; `series` for subject-less lines of one kind, and `line`
 * for a series of exactly one.
 */
export type SequenceFamily = "job" | "run" | "council" | "team_run" | "vcs" | "errand" | "series" | "line";

/** What a sequence's bar is filled with: how its newest line weighs, or a shadow decision, or nothing. */
export type SequenceShade = Exclude<FeedGravity, "routine"> | "shadow" | "routine";

export interface FeedSequence {
  key: string;
  /** `null` for a subject whose prefix this shell has never heard of — it is named by itself. */
  family: SequenceFamily | null;
  name: string;
  /** The project, or the errand for a line that is not the errand's own; `null` is the machine itself. */
  owner: string | null;
  lane: FeedLane;
  /** Oldest first. */
  lines: FeedEntry[];
  /** Epoch ms of the first and the newest line. */
  start: number;
  last: number;
  /** The newest line leaves it going — a parked job, a run with another attempt coming. */
  open: boolean;
  /** The worst any of its lines was: wrong over held over asks over routine. What a tally and a fold read. */
  gravity: FeedGravity;
  /**
   * The colour of how it ENDED — its newest line's gravity. A job whose gate failed and then
   * finished ends grey; the failure keeps its own solid mark on the bar.
   */
  shade: SequenceShade;
  /** How it ended, in the map's words for the newest line. */
  says: string;
  /** The same, without the words the row's name already says: "failed for good" beside "run 900598". */
  ending: string;
  /** Attempts made, when any line was a retry. */
  attempts: number | null;
}

const RANK: Record<FeedGravity, number> = { routine: 0, asks: 1, held: 2, wrong: 3 };
const FAMILIES: ReadonlySet<string> = new Set(["job", "run", "council", "team_run", "vcs", "errand"]);

/** The key a line is grouped under: its subject, else its run, else its errand, else its kind. */
export function sequenceKey(entry: FeedEntry): string {
  if (entry.subject !== null && entry.subject !== "") return entry.subject;
  if (entry.run_id !== null) return `run:${entry.run_id}`;
  if (entry.errand_id !== null) return `errand:${entry.errand_id}`;
  return `kind:${entry.kind}`;
}

function familyOf(key: string, count: number): { family: SequenceFamily | null; id: string } {
  const at = key.indexOf(":");
  const prefix = at === -1 ? key : key.slice(0, at);
  const id = at === -1 ? "" : key.slice(at + 1);
  if (prefix === "kind") return { family: count === 1 ? "line" : "series", id };
  return FAMILIES.has(prefix) ? { family: prefix as SequenceFamily, id } : { family: null, id };
}

/**
 * A label without the leading words a name already carries — "run failed for good" beside
 * "run 900598" reads "failed for good", "shadow run completed" beside "shadow run 900590" reads
 * "completed". Word by word from the front and nothing cleverer, so "worktree released" beside a
 * run keeps every word.
 */
function withoutName(label: string, name: string): string {
  const words = label.split(" ");
  const named = name.split(" ");
  let at = 0;
  while (at < words.length - 1 && words[at] === named[at]) at += 1;
  return words.slice(at).join(" ");
}

/**
 * A sequence's name, as a person would say it.
 *
 * The id is always there, because it is what the rest of the app searches by. A second part is
 * added only where a summary the núcleo writes carries one in a fixed shape — the rule a job was
 * started from, the errand's own name — and never guessed from free text.
 */
function nameOf(key: string, family: SequenceFamily | null, id: string, lines: FeedEntry[]): string {
  const extra = (pattern: RegExp) => {
    for (const line of lines) {
      const found = pattern.exec(line.summary);
      if (found !== null) return ` · ${found[1]}`;
    }
    return "";
  };
  switch (family) {
    case "job":
      return `job ${id}${extra(/from the rule (.+)$/)}`;
    case "run":
      return lines.some((line) => line.kind === "shadow_run_completed") ? `shadow run ${id}` : `run ${id}`;
    case "council":
      return `council ${id}`;
    case "team_run":
      return `team run ${id}`;
    case "vcs":
      return `vcs request ${id}`;
    case "errand":
      return `errand ${id}${extra(/of the errand "([^"]+)"/)}`;
    case "line":
    case "series":
      return readFeedKind(lines[0].kind)?.label ?? lines[0].kind;
    default:
      return key;
  }
}

/** The words for how a line left things: the map's label, or the wait's own reading for a parked job. */
export function endingWords(entry: FeedEntry): string {
  if (entry.kind === "job_waiting") {
    const reason = waitReasonFromSummary(entry.summary);
    const reading = reason === null ? null : readState("wait_reason", reason);
    if (reading !== null) return reading.label;
  }
  return readFeedKind(entry.kind)?.label ?? entry.kind;
}

const time = (entry: FeedEntry) => Date.parse(entry.created_at);

/**
 * Every line, grouped into sequences, ordered by when each began.
 *
 * A line without a subject is never open: "job started" with nothing to tie a finish to would be
 * drawn as going on forever, and the trace would be claiming work nobody can find.
 */
export function buildSequences(entries: FeedEntry[]): FeedSequence[] {
  const groups = new Map<string, FeedEntry[]>();
  for (const entry of entries) {
    const key = sequenceKey(entry);
    const group = groups.get(key);
    if (group === undefined) groups.set(key, [entry]);
    else group.push(entry);
  }

  const sequences: FeedSequence[] = [];
  for (const [key, group] of groups) {
    const lines = [...group].sort((a, b) => time(a) - time(b) || a.id - b.id);
    const { family, id } = familyOf(key, lines.length);
    const newest = lines[lines.length - 1];

    let gravity: FeedGravity = "routine";
    let weightiest: FeedEntry | null = null;
    for (const line of lines) {
      const weight = feedGravityOf(line.kind);
      if (RANK[weight] > RANK[gravity]) {
        gravity = weight;
        weightiest = line;
      }
    }
    const endedAs = feedGravityOf(newest.kind);
    const shade: SequenceShade =
      endedAs !== "routine" ? endedAs : feedMarkTone(newest.kind) === "shadow" ? "shadow" : "routine";

    const retries = lines.filter((line) => line.kind === "run_retry").length;
    const project = lines.find((line) => line.project_id !== null)?.project_id ?? null;
    const errand = family === "errand" ? null : (lines.find((line) => line.errand_id !== null)?.errand_id ?? null);
    // A series is many owners' lines of one kind; it has an owner only when they all share one.
    const shared = family !== "series" || lines.every((line) => line.project_id === project);
    const name = nameOf(key, family, id, lines);
    const says = endingWords(newest);

    sequences.push({
      key,
      family,
      name,
      owner: shared ? (project ?? (errand !== null ? `errand ${errand}` : null)) : null,
      lane: feedLaneOf((weightiest ?? lines[0]).kind),
      lines,
      start: time(lines[0]),
      last: time(newest),
      open: family !== "line" && family !== "series" && feedKindLeavesOpen(newest.kind),
      gravity,
      shade,
      says,
      ending: family === "line" || family === "series" || family === null ? says : withoutName(says, name),
      attempts: retries > 0 ? retries + 1 : null,
    });
  }
  return sequences.sort((a, b) => a.start - b.start || a.key.localeCompare(b.key));
}

/** More rows than this in one lane, and its routine sequences and series fold into a single row. */
export const LANE_FOLD_ABOVE = 12;

export interface TraceLane {
  lane: FeedLane;
  label: string;
  /** Every sequence in the lane, by when it began. */
  sequences: FeedSequence[];
  /** The ones drawn as rows of their own. */
  shown: FeedSequence[];
  /** Routine sequences folded into one row, when the lane is dense; empty otherwise. */
  folded: FeedSequence[];
  /** How many lines the lane holds. */
  lines: number;
}

/**
 * Sequences placed in their lanes, in the lanes' own order, with empty lanes left out.
 *
 * A dense lane keeps every sequence that went wrong, was held or asks for you as a row — and every
 * one still open, since "2 still open" in the header must point at two rows — and folds the rest
 * into one: a week of heartbeats is a count, and a failure folded into a count is a failure nobody
 * finds.
 */
export function traceLanes(sequences: FeedSequence[], foldAbove = LANE_FOLD_ABOVE): TraceLane[] {
  return FEED_LANES.flatMap((info) => {
    const inLane = sequences.filter((sequence) => sequence.lane === info.id);
    if (inLane.length === 0) return [];
    const dense = inLane.length > foldAbove;
    return [
      {
        lane: info.id,
        label: info.label,
        sequences: inLane,
        shown: dense ? inLane.filter((sequence) => !foldable(sequence)) : inLane,
        folded: dense ? inLane.filter(foldable) : [],
        lines: inLane.reduce((sum, sequence) => sum + sequence.lines.length, 0),
      },
    ];
  });
}

const foldable = (sequence: FeedSequence) => sequence.gravity === "routine" && !sequence.open;

/** Where a sequence stood at a moment of the replay. */
export type SequenceState = "queued" | "running" | "settled";

/**
 * A sequence at `at`, with `now` the right edge of the window.
 *
 * Before its first line it had not started. An open one is in progress from its first line to
 * now — a parked job is still parked. A closed one is in progress until its newest line and
 * settled from then on.
 */
export function sequenceStateAt(sequence: FeedSequence, at: number, now: number): SequenceState {
  if (at < sequence.start) return "queued";
  // Nothing runs between the lines of a series: once its first is written, it has said something.
  if (sequence.family === "series") return "settled";
  if (sequence.open) return "running";
  return at < Math.min(sequence.last, now) ? "running" : "settled";
}

/** How much of a sequence's bar is filled at `at`, from 0 to 1. */
export function sequenceProgressAt(sequence: FeedSequence, at: number, now: number): number {
  const end = sequence.open ? now : sequence.last;
  const extent = end - sequence.start;
  if (extent <= 0) return at >= sequence.start ? 1 : 0;
  return Math.min(Math.max((at - sequence.start) / extent, 0), 1);
}

/** The sequence a default selection lands on: the newest to have gone wrong, been held or asked. */
export function newestException(sequences: FeedSequence[]): FeedSequence | null {
  let best: FeedSequence | null = null;
  for (const sequence of sequences) {
    if (sequence.gravity === "routine") continue;
    if (best === null || sequence.last > best.last) best = sequence;
  }
  return best;
}
