// §spec mapa-do-projeto
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import type {
  Anchor,
  Anchored,
  Held,
  Junction,
  StampCounts,
  Standing,
  TriageCounts,
  Watch,
} from "../data/project-map";
import { renderWithQuery } from "../test/harness";
import { StampsPanel } from "./StampsPanel";

/**
 * One approved decision. The anchor rarely matters here — this panel reads the verdict axis.
 *
 * **Portuguese on purpose, and it is the row a real daemon would send.** The documents under
 * `.ai/specs/` are Portuguese by the owner's rule, so a decision parsed out of one arrives with a
 * Portuguese slug, a Portuguese heading and a Portuguese sentence. Translating the `text` alone
 * would leave an English sentence under `§7 O carimbo e a sua caducidade` inside a document called
 * `mapa-do-projeto` — a fixture testing a shape production cannot produce.
 *
 * The `Carimbar` it opens with is the **verb**, not the component beside it, which is `GiveStamp`.
 * Worth saying out loud because a rename sweep walks straight into this line and cannot tell the
 * two apart: one already did, and turned a spec sentence into *"GiveStamp grava o veredicto"*.
 */
function anchored(overrides: Partial<Anchored> = {}): Anchored {
  return {
    decision_id: 1,
    ordinal: 1,
    spec_slug: "2026-08-24-mapa-do-projeto-design",
    section: "§7 O carimbo e a sua caducidade",
    text: "Carimbar grava o veredicto, quando, o digest do codigo ancora e uma nota.",
    kind: "character",
    anchor: "ambiguous",
    modules: ["core/src/map_stamp.rs"],
    foreign: [],
    // Nobody has written down which files this decision's code is, which is every
    // decision on day one and the state each of these fixtures is about.
    record: null,
    ...overrides,
  };
}

/** A decision and where it stands, plus whatever the triager said about it. */
interface Row {
  row: Anchored;
  standing: Standing;
  /** Only ever set on a `never` row — triage describes nothing else (§10). */
  judged?: Held["judgement"];
}

function held(decisionId: number, judgement: Held["judgement"]): Held {
  return {
    decision_id: decisionId,
    judgement,
    reason: "O triador escreveu uma razao, porque a tabela exige uma.",
    model: "cloud",
    computed_at: "2026-08-26T09:00:00Z",
    inputs_digest: "abc123",
    machine_written: false,
    checked: true,
  };
}

function settled(watch: Watch): Standing {
  return { state: "settled", stamped_at: "2026-08-25T09:00:00Z", watch };
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
 * §5.3's header, **tallied from the standings the fixture actually holds** and never handed in.
 *
 * This mirrors `map_stamp::counts`, which the núcleo asserts reconciles —
 * `settled + partial + never + lapsed + withdrawn === decisions`, and `unwatched` is exactly the
 * three silences added up. A fixture free to write a disagreeing header would let this panel pass
 * while showing numbers no daemon could ever send, which is the false confidence of §1 rebuilt
 * inside the test suite that is supposed to catch it.
 */
function tally(standings: Record<string, Standing>): StampCounts {
  const rows = Object.values(standings);
  const inState = (state: Standing["state"]) => rows.filter((row) => row.state === state).length;
  const watching = (watch: Watch) =>
    rows.filter((row) => row.state === "settled" && row.watch === watch).length;
  const no_anchor = watching("no_anchor");
  const untracked = watching("untracked");
  const no_repository = watching("no_repository");
  return {
    settled: inState("settled"),
    partial: inState("partial"),
    never: inState("never"),
    lapsed: inState("lapsed"),
    withdrawn: inState("withdrawn"),
    guessed: watching("guessed"),
    no_anchor,
    untracked,
    no_repository,
    unwatched: no_anchor + untracked + no_repository,
    decisions: rows.length,
  };
}

/**
 * §5.3's `K` and `J` as `map_triage::reconcile` builds them, and never as a fixture's opinion.
 *
 * Every `never` decision falls in exactly one of `flagged`, `silenced` and `untriaged` in one pass;
 * `unseen` is the last two added up and `waiting` is `lapsed + flagged`. A fixture free to hand in a
 * disagreeing tally would let this panel pass while printing a header no daemon could ever send —
 * and the header is exactly where a reader stops checking.
 */
function triageTally(rows: Row[], standings: Record<string, Standing>): TriageCounts {
  const never = rows.filter((pair) => pair.standing.state === "never");
  const flagged = never.filter((pair) => pair.judged === "flagged").length;
  const silenced = never.filter((pair) => pair.judged === "silenced").length;
  const untriaged = never.length - flagged - silenced;
  const lapsed = Object.values(standings).filter((row) => row.state === "lapsed").length;
  return {
    flagged,
    silenced,
    untriaged,
    unseen: silenced + untriaged,
    waiting: lapsed + flagged,
    unchecked: 0,
  };
}

function open(rows: Row[], options: { gitWouldNotAnswer?: boolean; refusal?: unknown } = {}) {
  const standings: Record<string, Standing> = {};
  for (const { row, standing } of rows) standings[String(row.decision_id)] = standing;

  const triage: Record<string, Held> = {};
  for (const { row, judged } of rows) {
    if (judged !== undefined) triage[String(row.decision_id)] = held(row.decision_id, judged);
  }

  daemon.apiFetch.mockReset();
  if (options.refusal === undefined) daemon.apiFetch.mockResolvedValue(undefined);
  else daemon.apiFetch.mockRejectedValue(options.refusal);

  return renderWithQuery(
    <StampsPanel
      projectId="nucleos"
      junction={junction(rows.map((pair) => pair.row))}
      standings={standings}
      stamps={tally(standings)}
      triage={triage}
      triageCounts={triageTally(rows, standings)}
      gitWouldNotAnswer={options.gitWouldNotAnswer ?? false}
    />,
  );
}

describe("the owner's verdict, and what became of it", () => {
  /**
   * §5.3, and the whole of it: four numbers, in that order, in one sentence.
   *
   * The `%` assertion is not decoration. §12 refuses coverage as a percentage — *"um número único
   * é exactamente o colapso que o §5 proíbe"* — and this is the one panel with the numbers to
   * build one out of, so the ban is asserted here rather than hoped for. A bar or a ring is the
   * same collapse with no digits, so those are checked too.
   */
  it("shows the four numbers of §5.3 and never a percentage", () => {
    const { container } = open([
      { row: anchored({ decision_id: 1 }), standing: settled("watched") },
      { row: anchored({ decision_id: 2 }), standing: settled("watched") },
      {
        row: anchored({ decision_id: 3 }),
        standing: { state: "partial", stamped_at: "2026-08-25T09:00:00Z", note: "falta o painel" },
      },
      { row: anchored({ decision_id: 4 }), standing: { state: "never" } },
      { row: anchored({ decision_id: 5 }), standing: { state: "never" } },
      { row: anchored({ decision_id: 6 }), standing: { state: "never" } },
      {
        row: anchored({ decision_id: 7 }),
        standing: {
          state: "lapsed",
          stamped_at: "2026-08-25T09:00:00Z",
          why: { kind: "moved", changed: ["core/src/map_stamp.rs"], added: [], gone: [] },
        },
      },
    ]);

    expect(
      screen.getByText("2 stamped · 1 part-way · 3 never looked at · 1 on your desk"),
    ).toBeTruthy();
    expect(container.textContent ?? "").not.toContain("%");
    expect(container.querySelector("progress")).toBeNull();
    expect(container.querySelector("meter")).toBeNull();
    expect(screen.queryAllByRole("progressbar")).toHaveLength(0);
  });

  /**
   * §5.3, after slice 5 moved two of its four numbers: `J` is `lapsed + flagged` and `K` is
   * `never − flagged`. A flagged decision has arrived in front of the owner, so counting it in both
   * would put one decision on two lines of a header that is supposed to reconcile — and a header is
   * exactly where a reader stops checking.
   */
  it("counts a flagged decision on the desk and not in the debt", () => {
    open([
      { row: anchored({ decision_id: 1 }), standing: settled("watched") },
      { row: anchored({ decision_id: 2 }), standing: { state: "never" }, judged: "flagged" },
      { row: anchored({ decision_id: 3 }), standing: { state: "never" } },
      {
        row: anchored({ decision_id: 4 }),
        standing: {
          state: "lapsed",
          stamped_at: "2026-08-25T09:00:00Z",
          why: { kind: "moved", changed: ["core/src/map_stamp.rs"], added: [], gone: [] },
        },
      },
    ]);

    // Two never-stamped decisions, one of them flagged: one is debt and the other is on the desk
    // beside the lapse, which makes the desk two.
    expect(
      screen.getByText("1 stamped · 0 part-way · 1 never looked at · 2 on your desk"),
    ).toBeTruthy();
    // And the flagged one is not drawn here at all: it is on the triage panel with the reason that
    // put it there, and a second list of it would be the second panel answering one question.
    const debt = screen.getByLabelText("Decisions nobody has stamped");
    expect(within(debt).queryAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByText(/not stamps of yours at all/)).toBeTruthy();
  });

  /**
   * **The line the whole triage slice turns on.** §5.1: a silence is *"o triador não viu nada
   * estranho. Ninguém olhou. Não é verde."* — so it never leaves `K`, and the pile still draws it.
   * A triager that silenced three hundred decisions and drove the debt figure to zero would be §1's
   * false confidence manufactured by the arithmetic of its own cure, with the sum still reconciling
   * perfectly.
   */
  it("does not shrink the debt when the triager silences, and still draws what it silenced", () => {
    open([
      { row: anchored({ decision_id: 1 }), standing: { state: "never" }, judged: "silenced" },
      { row: anchored({ decision_id: 2 }), standing: { state: "never" }, judged: "silenced" },
      { row: anchored({ decision_id: 3 }), standing: { state: "never" } },
    ]);

    expect(
      screen.getByText("0 stamped · 0 part-way · 3 never looked at · 0 on your desk"),
    ).toBeTruthy();
    const debt = screen.getByLabelText("Decisions nobody has stamped");
    expect(within(debt).queryAllByRole("listitem")).toHaveLength(3);
    expect(screen.getByText(/2 of them the triager silenced/)).toBeTruthy();
    expect(screen.getByText(/Nobody has looked at them/)).toBeTruthy();
  });

  /**
   * A green that will never come back to ask is the shape of §1's false confidence, and today it
   * is the common case rather than the corner. So the count is printed and the three reasons are
   * kept apart: one is repaired by writing a citation, one by a `.gitignore` line **or** by a `git
   * add` — nothing can tell which — and one by nothing at all, because it is not a defect.
   */
  it("says how many stamps can never expire, and names which of the three reasons each one has", () => {
    open([
      { row: anchored({ decision_id: 1, section: "§8 A ancora" }), standing: settled("no_anchor") },
      { row: anchored({ decision_id: 2, section: "§9 Onde vive" }), standing: settled("untracked") },
      {
        row: anchored({ decision_id: 3, section: "§11 Projetos sem specs" }),
        standing: settled("no_repository"),
      },
    ]);

    expect(screen.getByText(/^3 of your greens are standing on something that cannot move/)).toBeTruthy();

    expect(screen.getByText(/^1 of them: no readable module names/)).toBeTruthy();
    expect(screen.getByText(/^1 of them: modules name the section and git reports none of them/)).toBeTruthy();
    expect(screen.getByText(/^1 of them: this project.s folder is not a git repository/)).toBeTruthy();

    // The rows sit under the reason that explains them, so the cure a reader reaches for is the
    // one that would actually work on the decision they are looking at.
    expect(
      within(screen.getByLabelText("Greens with nothing to watch")).getByText(/§8 A ancora/),
    ).toBeTruthy();
    expect(
      within(screen.getByLabelText("Greens whose files git does not track")).getByText(/§9 Onde vive/),
    ).toBeTruthy();
    expect(
      within(screen.getByLabelText("Greens in a folder with no repository")).getByText(
        /§11 Projetos sem specs/,
      ),
    ).toBeTruthy();

    // §11's case is not a fault and may not be worded as one.
    expect(screen.getByText(/there is nothing to repair/)).toBeTruthy();
    // `untracked` has two causes and the map cannot tell them apart, so it may not send anybody
    // to one of them.
    expect(screen.queryByText(/\.gitignore/)).toBeNull();
  });

  /**
   * §8: a `§` in a file here is a number and nothing else. A green over such an anchor does
   * expire — which is why it is not one of the silences above — but what it expires against is a
   * guess, and it can be tripped by a file that was never about this decision at all.
   */
  it("separates a guessed anchor from a certain one, and says a guessed one may expire for an unrelated file", () => {
    open([
      { row: anchored({ decision_id: 1, section: "§7 O carimbo" }), standing: settled("guessed") },
      { row: anchored({ decision_id: 2, section: "§3 A juncao" }), standing: settled("watched") },
    ]);

    expect(
      screen.getByText(/^1 of your greens are watching files matched by section number alone/),
    ).toBeTruthy();
    expect(screen.getByText(/unrelated file that happens to write the same/)).toBeTruthy();
    expect(
      screen.getByText(/^1 of your greens are watching an anchor this map is certain about/),
    ).toBeTruthy();

    const guessed = screen.getByLabelText("Greens over a guessed anchor");
    expect(within(guessed).getByText(/§7 O carimbo/)).toBeTruthy();
    expect(within(guessed).queryByText(/§3 A juncao/)).toBeNull();
  });

  /**
   * §5.2 makes the note the entire value of amber: *"falta migrar as páginas de pilar"* is worth
   * more than the colour is. A note behind a hover is a note nobody reads, which is amber with
   * extra steps — so it is asserted to be text on the row and asserted **not** to be a `title`.
   */
  it("makes the amber note visible on the row rather than behind a hover", () => {
    const NOTE = "falta migrar as paginas de pilar";
    open([
      {
        row: anchored({ decision_id: 4 }),
        standing: { state: "partial", stamped_at: "2026-08-25T09:00:00Z", note: NOTE },
      },
    ]);

    const shown = screen.getByText(NOTE);
    expect(shown).toBeTruthy();

    const item = shown.closest("li");
    expect(item).not.toBeNull();
    const hovers = Array.from(item?.querySelectorAll("[title]") ?? []);
    expect(hovers.some((el) => (el.getAttribute("title") ?? "").includes(NOTE))).toBe(false);
  });

  /**
   * §7: *"re-carimbar é um clique quando o diff é cosmético, e é o momento certo para olhar quando
   * não é"*. That judgement needs the paths, and it needs the three kinds kept apart — a file that
   * appeared asks a completely different question from one that changed.
   */
  it("names which files moved under a lapsed stamp", () => {
    open([
      {
        row: anchored({ decision_id: 5 }),
        standing: {
          state: "lapsed",
          stamped_at: "2026-08-25T09:00:00Z",
          why: {
            kind: "moved",
            changed: ["core/src/map_stamp.rs"],
            added: ["core/src/map_join.rs"],
            gone: ["core/src/old_map.rs"],
          },
        },
      },
    ]);

    expect(screen.getByText(/core\/src\/map_stamp\.rs/)).toBeTruthy();
    expect(screen.getByText(/core\/src\/map_join\.rs/)).toBeTruthy();
    expect(screen.getByText(/core\/src\/old_map\.rs/)).toBeTruthy();

    expect(screen.getByText(/changed since you stamped it/)).toBeTruthy();
    expect(screen.getByText(/appeared since you stamped it/)).toBeTruthy();
    expect(screen.getByText(/gone since you stamped it/)).toBeTruthy();
  });

  /**
   * *I could not look* is the only true answer of the three available, and it is the only one a
   * reader can act on. Drawing it as a `moved` with three empty lists would report that every
   * anchor vanished — a change nothing measured, which is the silently-wrong answer this whole
   * feature exists to stop.
   */
  it("tells unreadable apart from moved instead of reporting that every anchor vanished", () => {
    open([
      {
        row: anchored({ decision_id: 6 }),
        standing: {
          state: "lapsed",
          stamped_at: "2026-08-25T09:00:00Z",
          why: { kind: "unreadable" },
        },
      },
    ]);

    expect(screen.getByText(/could not read what this stamp is watching/)).toBeTruthy();
    expect(screen.queryByText(/gone since you stamped it/)).toBeNull();
    expect(screen.queryByText(/changed since you stamped it/)).toBeNull();
    expect(screen.queryByText(/appeared since you stamped it/)).toBeNull();
  });

  /**
   * §5.2: *"Fica retirada, com o spec marcado por actualizar… Retirar é uma afirmação, não um
   * esquecimento."* This pile is the marked-for-updating half, and without it withdrawing is just
   * forgetting — so the document that still claims the line has to be on screen beside it.
   */
  it("keeps a withdrawal visible and says which document still claims it", () => {
    open([
      {
        row: anchored({
          decision_id: 7,
          spec_slug: "2026-08-22-workspace-de-projeto-design",
          section: "§4 Tres modos",
          text: "Tres modos com formas diferentes, nao sete tabs.",
        }),
        standing: {
          state: "withdrawn",
          stamped_at: "2026-08-25T09:00:00Z",
          note: "o quarto modo entrou, e a regra era sobre forma",
        },
      },
    ]);

    expect(
      screen.getByText(
        "2026-08-22-workspace-de-projeto-design still claims 1 decision you have withdrawn.",
      ),
    ).toBeTruthy();
    expect(screen.getByText(/Tres modos com formas diferentes/)).toBeTruthy();
    expect(screen.getByText(/o quarto modo entrou/)).toBeTruthy();
  });

  /**
   * The table refuses an empty note and the daemon answers `400`, so an amber button with nowhere
   * to type is a button that can only fail — a worse answer than a field. It is disabled rather
   * than hidden: hiding a verdict would be deciding which of the three the owner is allowed to
   * give.
   */
  it("does not offer amber without somewhere to type the note", () => {
    const row = anchored({ decision_id: 8, section: "§5.2 Veredicto do dono" });
    open([{ row, standing: { state: "never" } }]);

    const name = `${row.spec_slug} ${row.section}`;
    const amber = screen.getByLabelText(`stamp ${name} as part-way`) as HTMLButtonElement;
    expect(amber.disabled).toBe(true);

    const field = screen.getByLabelText(`note for ${name}`);
    fireEvent.change(field, { target: { value: "falta o painel de carimbos" } });
    expect(amber.disabled).toBe(false);

    // The other two take a note and do not need one, so neither was ever disabled.
    expect((screen.getByLabelText(`stamp ${name} as what you want`) as HTMLButtonElement).disabled).toBe(
      false,
    );
  });

  /**
   * A list that quietly stops is the same defect the whole mode treats. The rows are cut and the
   * number never is.
   */
  it("says how many rows it hid when a pile is longer than the cap", () => {
    open(
      Array.from({ length: 15 }, (_, at) => ({
        row: anchored({ decision_id: at + 1, section: `§${at + 1} Uma seccao` }),
        standing: { state: "never" } as Standing,
      })),
    );

    expect(screen.getByText(/^15 approved decisions carry no verdict of yours/)).toBeTruthy();
    expect(within(screen.getByLabelText("Decisions nobody has stamped")).getAllByRole("listitem")).toHaveLength(12);
    expect(screen.getByText(/3 more not shown here/)).toBeTruthy();
  });

  /**
   * §10, and the day the owner first opens this: ~350 lines and not one stamp. The temptation is a
   * reassuring adjective; §5.3 says the number is debt and is supposed to be uncomfortable, so the
   * sentence says nothing is stamped and leaves it there.
   */
  it("says plainly that nothing is stamped, on a project on day one", () => {
    open([
      { row: anchored({ decision_id: 1 }), standing: { state: "never" } },
      { row: anchored({ decision_id: 2 }), standing: { state: "never" } },
    ]);

    expect(screen.getByText(/^Nothing here is stamped\./)).toBeTruthy();
    expect(screen.getByText("0 stamped · 0 part-way · 2 never looked at · 0 on your desk")).toBeTruthy();
  });

  /**
   * The digest is computed once for the union of every decision's anchors, so a git that will not
   * answer takes every settled stamp to `unreadable` at the same instant. That is a fact about the
   * reading and not about any decision, and 350 identical rows saying it is a wall of noise nobody
   * reads to the bottom of — which is how the one real lapse underneath goes unseen.
   */
  it("says once, not per row, that git would not answer", () => {
    const lapsed = (id: number): Row => ({
      row: anchored({ decision_id: id, section: `§${id} Uma seccao` }),
      standing: {
        state: "lapsed",
        stamped_at: "2026-08-25T09:00:00Z",
        why: { kind: "unreadable" },
      },
    });
    open([lapsed(1), lapsed(2), lapsed(3)], { gitWouldNotAnswer: true });

    expect(screen.getAllByText(/Git would not answer when this map was read/)).toHaveLength(1);
    // The rows are still three, so the sentence replaced nothing.
    expect(screen.getAllByText(/could not read what this stamp is watching/)).toHaveLength(3);
  });

  /**
   * `503` is git being busy, not the owner being wrong. §7.1 makes *está como quero* the only
   * verdict the code moving can falsify, so it is the only one that may not be recorded without
   * knowing what it is anchored to — and a second attempt works. Copy that read as a failure would
   * put the blame on the person who pressed the button.
   */
  it("reads a 503 as try again, not as a failure the owner caused", async () => {
    const row = anchored({ decision_id: 9, section: "§7 O carimbo" });
    open([{ row, standing: { state: "never" } }], {
      refusal: new ApiRefusal(503, "unavailable", ""),
    });

    fireEvent.click(screen.getByLabelText(`stamp ${row.spec_slug} ${row.section} as what you want`));

    const said = await screen.findByText(/try again in a moment/);
    expect(said.textContent ?? "").toMatch(/nothing you asked for was wrong/i);
    expect(said.textContent ?? "").not.toMatch(/failed/i);
  });

  /**
   * *Mudei de ideias* is one click away from every other row on this screen, and §9.2 makes the
   * current state the last row written — so a verdict is always revisable and the panel has to
   * offer the gesture. It did not in the first draft, which made a mis-click permanent as far as
   * anybody using it could tell: exactly the kind of trap that would cost the trust this whole
   * feature is buying.
   */
  it("lets a verdict be revised, a withdrawal included", () => {
    const row = anchored({ decision_id: 11, section: "§4 Tres modos" });
    open([
      {
        row,
        standing: { state: "withdrawn", stamped_at: "2026-08-25T09:00:00Z", note: null },
      },
    ]);

    const name = `${row.spec_slug} ${row.section}`;
    expect(screen.getByLabelText(`stamp ${name} as what you want`)).toBeTruthy();
    expect(screen.getByLabelText(`note for ${name}`)).toBeTruthy();
  });

  /**
   * The same, for a green whose anchor cannot move. Nothing on that pile is asking to be
   * re-stamped — its repairs are a citation, a git command and nothing at all — but §6 gives the
   * verdict to the owner, and withholding the gesture would be this panel deciding when they are
   * allowed to change their mind.
   */
  it("lets a green with nothing to watch be re-stamped", () => {
    const row = anchored({ decision_id: 13, section: "§11 Projetos sem specs" });
    open([{ row, standing: settled("no_repository") }]);

    expect(
      screen.getByLabelText(`stamp ${row.spec_slug} ${row.section} as changed your mind`),
    ).toBeTruthy();
  });

  /** What was sent, so a test can tell a stamp that landed from one that only looked like it did. */
  it("sends the decision, the verdict and the note the owner typed", async () => {
    const row = anchored({ decision_id: 12, section: "§5.2 Veredicto do dono" });
    open([{ row, standing: { state: "never" } }]);

    const name = `${row.spec_slug} ${row.section}`;
    fireEvent.change(screen.getByLabelText(`note for ${name}`), {
      target: { value: "  falta o painel  " },
    });
    fireEvent.click(screen.getByLabelText(`stamp ${name} as part-way`));

    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
    const [path, init] = daemon.apiFetch.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/projects/nucleos/map/stamps");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      decision_id: 12,
      verdict: "partial",
      note: "falta o painel",
    });
  });
});
