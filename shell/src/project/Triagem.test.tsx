import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import type {
  Age,
  Anchor,
  Anchored,
  Held,
  Junction,
  Recency,
  Silencing,
  TriageCounts,
  TriageReport,
} from "../data/project-map";
import { renderWithQuery } from "../test/harness";
import { Triagem } from "./Triagem";

/** One approved decision nobody has stamped, which is the only kind this panel ever draws. */
function anchored(overrides: Partial<Anchored> = {}): Anchored {
  return {
    decision_id: 1,
    ordinal: 1,
    spec_slug: "2026-08-24-mapa-do-projeto-design",
    section: "§6 O papel do modelo",
    text: "O modelo e compressor e triador, nunca certificador.",
    kind: "character",
    anchor: "ambiguous",
    modules: ["core/src/map_triage.rs"],
    foreign: [],
    ...overrides,
  };
}

function held(overrides: Partial<Held> = {}): Held {
  return {
    decision_id: 1,
    judgement: "flagged",
    reason: "O modulo cita a seccao e nao faz o que a decisao diz.",
    model: "cloud",
    computed_at: "2026-08-26T09:00:00Z",
    inputs_digest: "abc123",
    checked: true,
    ...overrides,
  };
}

function silencing(overrides: Partial<Silencing> = {}): Silencing {
  return {
    decision_id: 1,
    spec_slug: "2026-08-24-mapa-do-projeto-design",
    section: "§6 O papel do modelo",
    text: "O modelo e compressor e triador, nunca certificador.",
    reason: "Vi o modulo e bate certo com a decisao.",
    model: "cloud",
    computed_at: "2026-08-26T09:00:00Z",
    retired: false,
    machine_written: false,
    ...overrides,
  };
}

function report(overrides: Partial<TriageReport> = {}): TriageReport {
  return {
    in_scope: 0,
    already_current: 0,
    judged: 0,
    unreadable: 0,
    unanswered: 0,
    vanished: 0,
    left_over: 0,
    unreadable_anchors: 0,
    ...overrides,
  };
}

function junction(decisions: Anchored[]): Junction {
  const held = (anchor: Anchor) => decisions.filter((row) => row.anchor === anchor).length;
  return {
    decisions,
    unclaimed: [],
    unmatched: [],
    counts: {
      decisions: decisions.length,
      declared: held("declared"),
      ambiguous: held("ambiguous"),
      silent: held("silent"),
      unnumbered: held("unnumbered"),
      unclaimed: 0,
      unmatched: 0,
    },
  };
}

/**
 * §5.3's `K` and `J`, **tallied from the judgements the fixture actually holds** and never handed
 * in.
 *
 * This mirrors `map_triage::reconcile`, which the núcleo asserts reconciles by construction:
 * every decision nobody has stamped falls in exactly one of `flagged`, `silenced` and `untriaged`,
 * and `unseen` is the last two added up. A fixture free to write a disagreeing tally would let the
 * panel pass while showing numbers no daemon could ever send — and it would make the debt test
 * below prove nothing at all, since the number it watches would be the fixture's opinion rather
 * than the arithmetic §5.3 fixed.
 */
function tally(rows: Row[], lapsed = 0): TriageCounts {
  const judgements = rows.flatMap((pair) => (pair.held === undefined ? [] : [pair.held]));
  const inJudgement = (judgement: Held["judgement"]) =>
    judgements.filter((one) => one.judgement === judgement).length;
  const flagged = inJudgement("flagged");
  const silenced = inJudgement("silenced");
  const untriaged = rows.length - judgements.length;
  return {
    flagged,
    silenced,
    untriaged,
    unseen: silenced + untriaged,
    waiting: lapsed + flagged,
    unchecked: judgements.filter((one) => !one.checked).length,
  };
}

interface Row {
  row: Anchored;
  held?: Held;
}

interface Open {
  rows?: Row[];
  /** Which window the walk saw, and where each decision fell in it. */
  recency?: Recency;
  /** §6.2's pile, as its own route answers it. */
  pile?: Silencing[];
  /** What the run comes back with when the button is pressed. */
  answer?: TriageReport;
  /** Stamps that lapsed — invisible to this panel, and part of `waiting`. */
  lapsed?: number;
}

function open(options: Open = {}) {
  const rows = options.rows ?? [];
  const triage: Record<string, Held> = {};
  for (const { row, held } of rows) {
    if (held !== undefined) triage[String(row.decision_id)] = { ...held, decision_id: row.decision_id };
  }

  const recency: Recency = options.recency ?? {
    window: 200,
    ages: Object.fromEntries(rows.map(({ row }) => [String(row.decision_id), { state: "older" }])),
  };

  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path.endsWith("/map/silenced")) return options.pile ?? [];
    if (path.endsWith("/map/triage")) return options.answer ?? report();
    throw new Error(`no fixture for ${path}`);
  });

  return renderWithQuery(
    <Triagem
      projectId="nucleos"
      junction={junction(rows.map((pair) => pair.row))}
      triage={triage}
      counts={tally(rows, options.lapsed ?? 0)}
      recency={recency}
    />,
  );
}

/** The run, pressed and waited for — every report assertion needs it first. */
async function run() {
  fireEvent.click(screen.getByRole("button", { name: /run the triager with the cloud brain/ }));
  await waitFor(() => expect(screen.getByLabelText("What the run did")).toBeTruthy());
}

const moved = (at: number): Age => ({ state: "moved", at });

describe("what the triager thought, and what it never gets to decide", () => {
  /**
   * §5.1 names four derived states and this panel owns two of them. They have to be told apart on
   * screen, and — the half §6.1 spends a paragraph on — *silenciado* may never read as done:
   * *"se colapsassem, a autoridade que foi retirada ao modelo era-lhe devolvida pela porta da
   * renderização."* The other two states are the junction's and are drawn by `Juncao`; two panels
   * answering one question is the confusion this mode removes.
   */
  it("draws flagged and silenced apart, and never draws a silence as done", () => {
    open({
      rows: [
        {
          row: anchored({ decision_id: 1, section: "§6 O papel do modelo" }),
          held: held({ judgement: "flagged", reason: "Isto contradiz o que o codigo faz." }),
        },
        {
          row: anchored({ decision_id: 2, section: "§8 A ancora" }),
          held: held({ judgement: "silenced", reason: "Bate certo com o modulo." }),
        },
      ],
    });

    const flagged = screen.getByLabelText("Decisions the triager flagged");
    const silenced = screen.getByLabelText("Decisions the triager silenced");
    expect(within(flagged).getByText(/§6 O papel do modelo/)).toBeTruthy();
    expect(within(silenced).getByText(/§8 A ancora/)).toBeTruthy();
    // Each pile in its own list, or the two states are one pile with two labels.
    expect(within(flagged).queryByText(/§8 A ancora/)).toBeNull();
    expect(within(silenced).queryByText(/§6 O papel do modelo/)).toBeNull();

    // A silence is a claim about the triager and never about the code, and the panel has to say
    // so in words rather than leaving it to a colour nobody can name.
    expect(screen.getByText(/Nobody looked/)).toBeTruthy();
    expect(screen.getByText(/not green/i)).toBeTruthy();
    // No word on this panel may report a silence as a verdict. `settled`, `done` and `checked` are
    // the three a later tidy-up reaches for.
    const silencedText = silenced.textContent ?? "";
    expect(silencedText).not.toMatch(/\bdone\b|\bsettled\b|\bapproved\b/i);
  });

  /**
   * §6.2: the pile is readable *"com a razão de cada silenciamento e o modelo que o produziu"*,
   * because *"um triador que silencia o que não devia é um bug do triador, e um bug só é corrigível
   * se for visível"*. §13 rates that a **real** residual risk whose only mitigation is this. A
   * reason behind a hover is a bug report nobody reads.
   */
  it("puts the reason and the model on the row, with nothing to click", () => {
    const { container } = open({
      rows: [
        {
          row: anchored({ decision_id: 1 }),
          held: held({ judgement: "silenced", reason: "Bate certo com o modulo.", model: "local" }),
        },
      ],
    });

    expect(screen.getByText("Bate certo com o modulo.")).toBeTruthy();
    expect(screen.getByText(/Said by the local brain/)).toBeTruthy();
    // Not behind a disclosure, and not in a `title`.
    expect(container.querySelector("details")).toBeNull();
    expect(container.querySelector('[title="Bate certo com o modulo."]')).toBeNull();
  });

  /**
   * A reason opening `nucleos:` was written by this daemon and not by a model: it is the record of
   * an answer nobody could parse, flagged rather than dropped. `map_triage.model` names the brain
   * that **answered**, which stays true of an unreadable answer — so presenting the sentence under
   * that name, unqualified, is a machine's failure note wearing a model's opinion, which is the
   * attribution §6.2 exists to protect, inverted.
   */
  it("says when the daemon wrote a reason, and never attributes it to the model", () => {
    open({
      rows: [
        {
          row: anchored({ decision_id: 1 }),
          held: held({
            judgement: "flagged",
            reason: "nucleos: o modelo respondeu e ninguem conseguiu ler a resposta.",
            model: "cloud",
          }),
        },
      ],
    });

    const row = within(screen.getByLabelText("Decisions the triager flagged")).getByRole("listitem");
    expect(within(row).getByText(/written by NucleOS/i)).toBeTruthy();
    // The model is still named — it is the machine that was asked — but never as the author.
    expect(within(row).getByText(/cloud/)).toBeTruthy();
    expect(row.textContent ?? "").not.toMatch(/cloud said/i);
  });

  /**
   * `checked: false` means the digest could not be **computed**, never that it failed to match: git
   * would not say what the anchor code is. That is neither *this answer still stands* nor *this
   * answer has expired*, and guessing either is the error. Dropping the row is the worse of the two
   * guesses — it turns a judgement somebody's model actually gave into a claim that nobody ever
   * looked, over the whole project at once, every time git hiccups.
   */
  it("says a judgement could not be re-checked, and neither hides it nor shows it as verified", () => {
    open({
      rows: [
        {
          row: anchored({ decision_id: 1, section: "§7 O carimbo" }),
          held: held({ judgement: "flagged", checked: false }),
        },
      ],
    });

    const row = within(screen.getByLabelText("Decisions the triager flagged")).getByRole("listitem");
    expect(within(row).getByText(/§7 O carimbo/)).toBeTruthy();
    expect(within(row).getByText(/could not re-check/i)).toBeTruthy();
    expect(row.textContent ?? "").not.toMatch(/verified|confirmed/i);
  });

  /**
   * §10 offers this ordering as *um facto do git*, and the honest way to keep that true is to say
   * how far the git looked. A decision whose anchors last moved a thousand commits ago and one
   * whose anchors never moved sort the same here, and an unannounced approximation presented with
   * the confidence of a ranking is the false confidence of §1 arriving through the ordering.
   */
  it("says how far back the ordering can see", () => {
    open({
      rows: [{ row: anchored({ decision_id: 1 }), held: held() }],
      recency: { window: 200, ages: { "1": moved(1_756_200_000) } },
    });

    expect(screen.getByText(/last 200 commits/)).toBeTruthy();
  });

  /**
   * Measured against this repository: 71 of 80 decisions land inside the window carrying 19
   * distinct timestamps between them, and **the top eighteen share one**. A flat ranked list would
   * imply an ordering that is not there. The cause is §8 — while every citation is ambiguous a
   * decision's anchor set is every module citing that section *number* across all forty documents,
   * so the most recent of forty-two files in a repository committing thirty-five times a day is
   * this morning — and slice 6 is what narrows it, so this panel is honest about it meanwhile.
   */
  it("draws a tie in recency as a tie", () => {
    open({
      rows: [
        { row: anchored({ decision_id: 1, section: "§1 alfa" }), held: held({ decision_id: 1 }) },
        { row: anchored({ decision_id: 2, section: "§2 beta" }), held: held({ decision_id: 2 }) },
        { row: anchored({ decision_id: 3, section: "§3 gama" }), held: held({ decision_id: 3 }) },
      ],
      recency: {
        window: 200,
        ages: { "1": moved(1_756_200_000), "2": moved(1_756_200_000), "3": moved(1_756_100_000) },
      },
    });

    // The two that moved together are announced as tied, and the sentence says the panel is not
    // ordering them — not that they happen to be adjacent.
    expect(screen.getByText(/2 .*moved at the same moment/)).toBeTruthy();
    expect(screen.getByText(/nothing here orders/i)).toBeTruthy();
  });

  /**
   * `ForeignOnly` is *no readable module names this, and code this map cannot parse does*. It must
   * not read as *nothing implements this* — that is the failure `Structure::foreign` was added to
   * prevent, on a new axis, and six section numbers in this repository are in that state today.
   */
  it("does not read a decision only the sidecars name as one nothing implements", () => {
    open({
      rows: [
        {
          row: anchored({ decision_id: 1, modules: [], foreign: ["sidecars/web/main.go"] }),
          held: held(),
        },
      ],
      recency: { window: 200, ages: { "1": { state: "foreign_only" } } },
    });

    const band = screen.getByText(/cannot read/i);
    expect(band).toBeTruthy();
    const whole = screen.getByLabelText("The triager").textContent ?? "";
    expect(whole).not.toMatch(/nothing implements|nothing names them|no code at all/i);
  });

  /**
   * `left_over` is the field the report exists for. The batch is capped and this repository
   * normally saturates it — 17 of the 109 `§`-naming files were touched in the last 20 commits, and
   * an anchor set is several files — so a run that truncated and said nothing reads as *covered
   * everything* when it did not: §1's failure, produced by the feature built to cure it.
   */
  it("states what the run left over, whether or not anything was left over", async () => {
    open({ answer: report({ in_scope: 40, judged: 20, left_over: 17 }) });
    await run();
    expect(screen.getByText(/17 .*the batch cap stopped/)).toBeTruthy();
  });

  it("says so plainly when nothing was left over", async () => {
    open({ answer: report({ in_scope: 3, judged: 3 }) });
    await run();
    expect(screen.getByText(/Nothing was left over/)).toBeTruthy();
  });

  /**
   * *"Corri o triador e não aconteceu nada"* and *"corri o triador e estava tudo em dia"* are
   * different facts about a project, and a run reporting only its successes makes the second look
   * like the first.
   */
  it("tells a run that found nothing stale apart from a run with nothing to look at", async () => {
    const current = open({ answer: report({ in_scope: 12, already_current: 12 }) });
    await run();
    expect(screen.getByText(/already carried a judgement/)).toBeTruthy();
    expect(screen.queryByText(/every approved decision already carries a verdict of yours/)).toBeNull();
    current.unmount();

    open({ answer: report({ in_scope: 0 }) });
    await run();
    expect(screen.getByText(/every approved decision already carries a verdict of yours/)).toBeTruthy();
  });

  /**
   * **The line this whole slice turns on.** §5.1: silenced is *"o triador não viu nada estranho.
   * Ninguém olhou. Não é verde."* — so a silence is still debt nobody has given a verdict on, and
   * §5.3 authorises exactly one departure from `K`, which is about the flagged. A triager that
   * silences three hundred decisions and drives the debt figure to zero is §1's false confidence
   * manufactured by the cure's own arithmetic, on the one line that exists to be uncomfortable.
   *
   * The two fixtures differ **only** in whether the triager silenced, and the number the panel
   * prints may not move between them. A panel printing `untriaged` instead of `unseen` fails here
   * and looks entirely correct on screen.
   */
  it("does not shrink the number nobody has looked at when the triager silences", () => {
    // Read off the one sentence that carries the debt, and not off the whole panel: a silenced
    // pile prints its own count three lines up, so a regex over the section text matches that one
    // and passes against a panel printing `untriaged` here. Measured — the first version of this
    // test did exactly that and let the wrong field through.
    const debt = () => screen.getByText(/nobody has ever given a verdict on/).textContent ?? "";

    const backlog = [1, 2, 3, 4, 5].map((id) => ({ row: anchored({ decision_id: id }) }));
    const untouched = open({ rows: backlog });
    expect(debt()).toMatch(/^5 decisions nobody has ever given a verdict on\./);
    untouched.unmount();

    open({
      rows: backlog.map(({ row }) => ({
        row,
        held: held({ decision_id: row.decision_id, judgement: "silenced" }),
      })),
    });
    expect(debt()).toMatch(/^5 decisions nobody has ever given a verdict on\./);
  });

  /**
   * `unchecked` is a subset of `flagged + silenced` and not of the debt, so a project whose every
   * judgement is a flag has nothing in `unseen` and can still be holding judgements this reading
   * could not re-verify. That number is what says how much of the panel rests on an answer nobody
   * could test, and it may not vanish because a different number happens to be zero.
   */
  it("still says how many judgements went unchecked when there is no debt at all", () => {
    open({
      rows: [
        {
          row: anchored({ decision_id: 1 }),
          held: held({ judgement: "flagged", checked: false }),
        },
      ],
    });

    expect(screen.getByText(/1 of the judgements above could not be re-checked/)).toBeTruthy();
  });

  /**
   * §12 refuses coverage as a percentage outright — *"um número único é exactamente o colapso que o
   * §5 proíbe"* — and this panel is tempted by it: it holds three buckets of one population and a
   * report of seven counts, which is everything a ratio needs. A bar or a ring is the same collapse
   * with no digits.
   */
  it("shows no percentage, no bar and no score", async () => {
    const { container } = open({
      rows: [
        { row: anchored({ decision_id: 1 }), held: held({ judgement: "flagged" }) },
        {
          row: anchored({ decision_id: 2 }),
          held: held({ decision_id: 2, judgement: "silenced" }),
        },
        { row: anchored({ decision_id: 3 }) },
      ],
      answer: report({ in_scope: 3, judged: 3 }),
    });
    await run();

    expect(container.textContent ?? "").not.toContain("%");
    expect(container.querySelector("progress")).toBeNull();
    expect(container.querySelector("meter")).toBeNull();
    expect(screen.queryAllByRole("progressbar")).toHaveLength(0);
  });

  /**
   * An empty pile under a heading is indistinguishable from a pile the triager looked at and found
   * nothing in, and reading the first as the second is the false confidence this mode cures. What
   * the map can actually see is that no judgement describes anything here and nothing was ever
   * silenced, so the sentence says that and says what it infers from it.
   */
  it("says plainly when the triager has never run here", async () => {
    open({ rows: [{ row: anchored({ decision_id: 1 }) }] });

    await waitFor(() => expect(screen.getByText(/it has never run/)).toBeTruthy());
    expect(screen.queryByLabelText("Decisions the triager flagged")).toBeNull();
    expect(screen.queryByLabelText("Decisions the triager silenced")).toBeNull();
  });

  /**
   * §6.2 asks for *"a razão de **cada** silenciamento"*, and the map's own `triage` deliberately
   * drops three kinds of them: a decision stamped since, code moved since, a decision retired. Each
   * of those is a silencing worth **more** afterwards, not less — the stamp is the evidence the
   * silence was premature — so the pile has a door of its own and the panel walks through it.
   */
  it("shows a silencing the map itself no longer holds, and says why it is there", async () => {
    open({
      rows: [{ row: anchored({ decision_id: 1 }) }],
      pile: [
        silencing({ decision_id: 9, section: "§4 Fabricar a camada", retired: true }),
      ],
    });

    const record = await screen.findByLabelText("Silencings this map no longer holds");
    expect(within(record).getByText(/§4 Fabricar a camada/)).toBeTruthy();
    expect(within(record).getByText(/retired/i)).toBeTruthy();
  });

  /**
   * §5.2 has three verdicts and there is not a fourth. A flag the owner has read and decided is
   * noise leaves through *a meio, e eu sei* — *"converte um não sabia num sabia, que é metade da
   * cura"* — with a note saying so. A dismiss button would be the *não sabia* this map exists to
   * convert, kept and made quiet.
   */
  it("offers the stamp as the way out of a flag, and no dismiss", () => {
    open({
      rows: [{ row: anchored({ decision_id: 1 }), held: held({ judgement: "flagged" }) }],
    });

    const row = within(screen.getByLabelText("Decisions the triager flagged")).getByRole("listitem");
    expect(
      within(row).getByRole("button", { name: /stamp .* as part-way/ }),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: /dismiss|ignore|not now/i })).toBeNull();
    expect(screen.getByText(/half the cure/)).toBeTruthy();
  });
});
