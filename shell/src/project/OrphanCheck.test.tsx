// §spec mapa-do-projeto
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import type { Anchored, Loss, Orphan } from "../data/project-map";
import { renderWithQuery } from "../test/harness";
import { OrphanCheck } from "./OrphanCheck";

/** One approved decision nothing in the project names — the only row this control appears on. */
function anchored(overrides: Partial<Anchored> = {}): Anchored {
  return {
    decision_id: 1,
    ordinal: 1,
    spec_slug: "2026-08-24-mapa-do-projeto-design",
    section: "## 5.1 Estado derivado",
    text: "Uma decisao sem codigo nenhum e indistinguivel de uma que nunca foi extraida.",
    kind: "character",
    anchor: "silent",
    modules: [],
    foreign: [],
    // Nobody has written down which files this decision's code is, which is every
    // decision on day one and the state each of these fixtures is about.
    record: null,
    ...overrides,
  };
}

function loss(overrides: Partial<Loss> = {}): Loss {
  return {
    path: "core/src/map_join.rs",
    commit: "2a4d4af1a332feec8f0f4fba1fe81ea4cca8b28d",
    at: 1_787_788_755,
    subject: "refactor: rewrite the module header",
    renamed_to: null,
    declared: false,
    ...overrides,
  };
}

function open(row: Anchored = anchored()) {
  return renderWithQuery(<OrphanCheck projectId="alpha" row={row} />);
}

function press() {
  fireEvent.click(screen.getByRole("button", { name: /was this ever named/i }));
}

describe("whether a decision with no code lost the comment that anchored it", () => {
  // The mock is module-scoped, so its call history outlives a test. Two of the tests below count
  // calls to prove WHICH request a press made, and one proves that opening the panel makes none at
  // all — all three are assertions about a single press, and none of them means anything measured
  // against a running total.
  beforeEach(() => {
    daemon.apiFetch.mockReset();
  });

  /**
   * §14.3, and the reason this is a mutation and not a query.
   *
   * *"A pedido, por decisão, e nunca em fundo"*. The pile this control sits in is the headline of
   * the panel and can hold dozens of rows; a query would walk the repository's history once per
   * row, every time the mode opened, for a question almost nobody asks. Asserting the absence
   * because it is an absence — the thing a later refactor to `useQuery` would restore without
   * anything on screen looking different.
   */
  it("asks git nothing until somebody presses", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "never_named" } satisfies Orphan);

    open();

    expect(daemon.apiFetch).not.toHaveBeenCalled();
    press();
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledTimes(1));
  });

  /**
   * The heading travels verbatim, and the section NUMBER is never computed here.
   *
   * `map_join::section_number` is the one place that knows how `## 5.1 Estado derivado` becomes
   * `5.1`. A client that sent the number would be a second implementation of that rule, and the day
   * the two disagreed this guard would be searching the history for a section the junction never
   * anchored anything to — which is the same divergence §14.4 refuses between the past and the
   * present, one layer up.
   */
  it("sends the heading as it stands and never a number read off it", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "never_named" } satisfies Orphan);

    open();
    press();

    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledTimes(1));
    const asked = String(daemon.apiFetch.mock.calls[0][0]);
    expect(asked).toContain("/projects/alpha/map/orphan");
    expect(asked).toContain(`section=${encodeURIComponent("## 5.1 Estado derivado")}`);
    expect(asked).toContain("slug=2026-08-24-mapa-do-projeto-design");
  });

  it("names the file and the commit that took the citation away", async () => {
    daemon.apiFetch.mockResolvedValue({
      state: "lost",
      losses: [loss()],
    } satisfies Orphan);

    open();
    press();

    const said = await screen.findByRole("list", { name: /where the citation went/i });
    expect(said.textContent).toContain("core/src/map_join.rs");
    expect(said.textContent).toContain("refactor: rewrite the module header");
    // The whole object id and never an abbreviation: this sentence exists to be pasted into
    // `git show`, and an abbreviation is a hash that stops working when the repository grows.
    expect(said.textContent).toContain("2a4d4af1a332feec8f0f4fba1fe81ea4cca8b28d");
  });

  it("the commit date is in the shell's locale", async () => {
    const known = loss();
    daemon.apiFetch.mockResolvedValue({ state: "lost", losses: [known] } satisfies Orphan);

    open();
    press();

    expect((await screen.findByRole("listitem")).textContent).toContain(
      new Date(known.at * 1000).toLocaleDateString("en-GB"),
    );
  });

  /**
   * A loss is a fact about git, and the surface may not let it read as a verdict.
   *
   * The parser cannot tell *this implements §7.1* from *as §7.1 explains* — which is why
   * `map_join::Anchor::Declared` says **names** and never **implements** — so a panel that reported
   * a removed comment as a removed implementation would be manufacturing exactly the confidence §1
   * describes, through the rendering door §6.1 watches.
   */
  it("says a removed comment is not a removed implementation", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "lost", losses: [loss()] } satisfies Orphan);

    open();
    press();

    expect(await screen.findByText(/not a verdict/i)).toBeTruthy();
  });

  it("says where a renamed file went", async () => {
    daemon.apiFetch.mockResolvedValue({
      state: "lost",
      losses: [loss({ renamed_to: "core/src/map_orphan.rs" })],
    } satisfies Orphan);

    open();
    press();

    const said = await screen.findByRole("list", { name: /where the citation went/i });
    expect(said.textContent).toContain("that commit moved it to core/src/map_orphan.rs");
  });

  /**
   * The distinction the whole guard exists for, on the surface that can erase it.
   *
   * *I did not find it* and *I did not search all of it* are different facts. On a repository
   * longer than the window `not_in_window` is the ONLY negative that ever arrives, so drawing it
   * like `never_named` would assert *this was never built* about every unimplemented decision on
   * every mature project — a silently wrong answer, at scale, with nothing on screen saying so.
   */
  it("never draws a partial search as a complete one", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "not_in_window", window: 1000 } satisfies Orphan);

    open();
    press();

    const said = await screen.findByText(/last 1000 commits/i);
    expect(said.textContent).toMatch(/older history this did not read/i);
    // The sentence `never_named` would have put here, asserted absent by its own words rather than
    // by the phrase "never named" — which this very paragraph writes on purpose, to say which of
    // the two it is NOT.
    expect(screen.queryByText(/the comment was not deleted/i)).toBeNull();
  });

  it("says plainly when the row should not have been asked about", async () => {
    daemon.apiFetch.mockResolvedValue({
      state: "still_named",
      paths: ["shell/src/project/ModeMap.tsx"],
    } satisfies Orphan);

    open();
    press();

    const said = await screen.findByText(/names it right now/i);
    expect(said.textContent).toContain("shell/src/project/ModeMap.tsx");
  });

  /**
   * The two failures git can have are two sentences, and neither is a finding.
   *
   * `unreadable` is worth retrying and `no_repository` never will be — the same distinction
   * `map_stamp::Watch::NoRepository` exists for (§11). Telling somebody with no repository to try
   * again is advice that can never work, and telling somebody whose git hiccuped that their project
   * has none is worse.
   */
  it("says a git that would not answer is worth trying again, and leaves a way to", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "unreadable" } satisfies Orphan);

    open();
    press();

    expect((await screen.findByText(/would not answer/i)).textContent).toMatch(/trying again/i);
    // Advice nobody can act on is worse than none: the sentence says to try again, so the control
    // that would has to still be there.
    expect(screen.getByRole("button", { name: /ask git again/i })).toBeTruthy();
  });

  /**
   * §11's distinction, on the surface where it decides what somebody can press.
   *
   * `map_stamp::Watch::NoRepository` exists because *I could not look* carries exactly one piece of
   * advice — try again — and a project added from outside a repository will never succeed at
   * trying. A retry button here would be that advice, drawn.
   */
  it("does not tell a project with no repository to try again", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "no_repository" } satisfies Orphan);

    open();
    press();

    const said = await screen.findByText(/no repository/i);
    expect(said.textContent).toMatch(/nothing to retry/i);
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  /**
   * A settled answer is not re-asked, and that is the cost argument (§14.3) surviving contact with
   * a control.
   *
   * The four real answers are facts about a history that is not going to change between two clicks;
   * a button that stayed would invite a walk of the whole repository for the same sentence.
   */
  it("offers no way to re-ask a question the history has already answered", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "never_named" } satisfies Orphan);

    open();
    press();

    await screen.findByText(/ever named this section/i);
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  /**
   * §14 closing its own loop: what the history found becomes something no rewrite can delete.
   *
   * The guard has just named the file. Making the owner retype it would be the one place this
   * feature asked for work it could do itself.
   */
  it("writes down what the history found, in one press", async () => {
    daemon.apiFetch.mockResolvedValue({ state: "lost", losses: [loss()] } satisfies Orphan);

    open();
    press();
    await screen.findByRole("list", { name: /where the citation went/i });

    daemon.apiFetch.mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole("button", { name: /write this file down/i }));

    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledTimes(2));
    const [uri, init] = daemon.apiFetch.mock.calls[1];
    expect(String(uri)).toBe("/projects/alpha/map/anchors");
    expect(JSON.parse(String((init as { body: string }).body))).toEqual({
      decision_id: 1,
      paths: ["core/src/map_join.rs"],
    });
  });

  /**
   * **The destination path and not the one that lost the citation**, when the commit renamed it.
   *
   * `path` is where the citation WAS. A record has to name a file that is there NOW — the núcleo
   * refuses one that is not, and a record naming a vanished path would otherwise sit in the stamp
   * diff as a permanent `gone` for a file nobody deleted.
   */
  it("writes down where the file went and not where it was", async () => {
    daemon.apiFetch.mockResolvedValue({
      state: "lost",
      losses: [loss({ renamed_to: "core/src/map_orphan.rs" })],
    } satisfies Orphan);

    open();
    press();
    await screen.findByRole("list", { name: /where the citation went/i });

    daemon.apiFetch.mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole("button", { name: /write this file down/i }));

    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledTimes(2));
    const [, init] = daemon.apiFetch.mock.calls[1];
    expect(JSON.parse(String((init as { body: string }).body)).paths).toEqual([
      "core/src/map_orphan.rs",
    ]);
  });
  /**
   * A refusal is not an answer, and must not read as one.
   *
   * A `422` here means the heading carries no number, which is `unnumbered` — no search could run.
   * Falling through to *nothing ever named it* would report the result of a search that never
   * happened, on the one panel built to stop exactly that.
   */
  it("does not let a refused question read as nothing having been found", async () => {
    daemon.apiFetch.mockRejectedValue(
      new ApiRefusal(422, "unprocessable", "the daemon's own prose, which this branch overrides"),
    );

    open();
    press();

    expect(await screen.findByText(/carries no number/i)).toBeTruthy();
    expect(screen.queryByText(/the comment was not deleted/i)).toBeNull();
  });
});
