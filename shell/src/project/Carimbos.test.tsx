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
  Junction,
  StampCounts,
  Standing,
  Watch,
} from "../data/project-map";
import { renderWithQuery } from "../test/harness";
import { Carimbos } from "./Carimbos";

/** One approved decision. The anchor rarely matters here — this panel reads the verdict axis. */
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
    ...overrides,
  };
}

/** A decision and where it stands, which is the only pairing this panel draws. */
interface Row {
  row: Anchored;
  standing: Standing;
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

function open(rows: Row[], options: { gitWouldNotAnswer?: boolean; refusal?: unknown } = {}) {
  const standings: Record<string, Standing> = {};
  for (const { row, standing } of rows) standings[String(row.decision_id)] = standing;

  daemon.apiFetch.mockReset();
  if (options.refusal === undefined) daemon.apiFetch.mockResolvedValue(undefined);
  else daemon.apiFetch.mockRejectedValue(options.refusal);

  return renderWithQuery(
    <Carimbos
      projectId="nucleos"
      junction={junction(rows.map((pair) => pair.row))}
      standings={standings}
      stamps={tally(standings)}
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
