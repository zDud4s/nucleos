// §spec alcada-por-projecto
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import {
  DECLARED_ON,
  daemonFetch,
  daemonState,
  renderWithQuery,
  shellRule,
  type DaemonState,
} from "../test/harness";
import { ModeGithub } from "./ModeGithub";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

/** Mount the mode over a daemon holding exactly these declarations. */
function open(overrides: Partial<DaemonState> = {}): DaemonState {
  const state = daemonState(overrides);
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(daemonFetch(state));
  renderWithQuery(<ModeGithub projectId="nucleos" />);
  return state;
}

describe("what runs on its own", () => {
  /**
   * **The assertion the design argues hardest for.**
   *
   * §5.2: *"o que está fora do tecto é desenhado como facto e não como controlo … uma caixa que não
   * funcionasse seria uma mentira sobre quem decide"*. `declarable: false` is a fact about the
   * build — the compiled ceilings do not admit the operation, and no route, no file and no owner
   * can change that — so the row must carry the operation's name and nothing anybody can press.
   *
   * Asserted as the ABSENCE of any control inside the row, and not as a snapshot: a snapshot goes
   * green on whatever is rendered, including a disabled checkbox, which is precisely the lie this
   * rule forbids. A disabled control still says "this is a switch, and it is off" — the truth is
   * that there is no switch.
   */
  it("draws an operation outside the ceiling as a fact with no control at all", async () => {
    open();

    const row = await screen.findByRole("listitem", { name: "operation api_read" });

    // Its name is there — omitting it would claim this daemon cannot do it at all, which is the
    // other bad answer the catalogue's `declarable` flag exists to avoid.
    expect(row.textContent).toContain("api_read");
    expect(row.textContent).toContain("outside the compiled ceiling");

    // And nothing in it can be pressed, ticked or focused. Not "is disabled" — absent.
    expect(within(row).queryByRole("checkbox")).toBeNull();
    expect(within(row).queryByRole("button")).toBeNull();
    expect(within(row).queryByRole("switch")).toBeNull();
    expect(row.querySelector("input")).toBeNull();
    expect(row.querySelector("button")).toBeNull();

    // The comparison that gives the assertion its teeth: an operation the ceilings DO admit gets
    // the control this one is denied, from the same list and the same renderer.
    const admitted = screen.getByRole("listitem", { name: "operation run_list" });
    expect(within(admitted).getByRole("checkbox")).toBeDefined();
  });

  it("ticks the operations this project has declared, and only those", async () => {
    open({ githubOps: ["run_list"] });

    const declared = await screen.findByRole("listitem", { name: "operation run_list" });
    expect(within(declared).getByRole<HTMLInputElement>("checkbox").checked).toBe(true);

    const not = screen.getByRole("listitem", { name: "operation pr_list" });
    expect(within(not).getByRole<HTMLInputElement>("checkbox").checked).toBe(false);
  });

  it("grants an operation and reads the list back", async () => {
    const state = open();

    const row = await screen.findByRole("listitem", { name: "operation pr_list" });
    fireEvent.click(within(row).getByRole("checkbox"));

    await waitFor(() => expect(state.githubOps).toEqual(["pr_list"]));
    expect(state.policyWrites[0]).toMatchObject({
      method: "POST",
      body: { op_kind: "pr_list" },
    });
  });

  /**
   * The two halves are not in force in the same way, and the page has to say which.
   *
   * A declared read is consulted at the next decision through the Bash door; a declared action is
   * stored and awaits a later step. Drawing them identically would promise an owner their declared
   * `pr_comment` is running unattended, which it is not.
   */
  it("says an action is recorded and inert, and that a read is not", async () => {
    open();

    const actions = await screen.findByText(/recorded and inert/i);
    expect(actions.textContent).toContain("later step");

    expect(screen.getByText(/consulted at the next decision/i)).toBeDefined();
  });
});

describe("what the worktrees may run", () => {
  /**
   * §5.3 asks for the precedence *on the page* and not only in the code, and the reason is that the
   * two lists are not mirror images: somebody reading them as two ends of one switch writes an
   * `allow` expecting it to lift a refusal.
   */
  it("writes the precedence rule on the page", async () => {
    open();

    // The section's heading is on screen before its query answers, so the rule is read only once
    // the lists themselves are — otherwise this asserts against "Reading this project's rules…".
    await screen.findByLabelText("Command prefix");
    const said =
      screen.getByRole("region", { name: "What the worktrees may run" }).textContent ?? "";

    // `deny` wins, over an `allow` and over a compiled permission.
    expect(said).toMatch(/deny\s*beats\s*allow/i);
    expect(said).toContain("compiled into the núcleo");
    // And the half nobody guesses: an allow widens what would have ASKED, never what refuses.
    expect(said).toMatch(/never lifts a refusal/i);
    expect(said).toContain("rm -rf /");
  });

  it("shows the folded prefix, because that is what is stored and enforced", async () => {
    open({ shellRules: [shellRule({ prefix: "remove-item -recurse", verdict: "deny" })] });

    expect(
      await screen.findByRole("listitem", { name: "rule remove-item -recurse" }),
    ).toBeDefined();
  });

  it("previews what the núcleo will store for a prefix somebody is typing", async () => {
    open();

    const box = await screen.findByLabelText("Command prefix");
    fireEvent.change(box, { target: { value: "Remove-Item  -Recurse" } });

    const preview = screen.getByText(/stored and enforced as/i);
    expect(preview.textContent).toContain("remove-item -recurse");
  });

  /**
   * The caption is the day the prefix was FIRST declared, in the daemon's own spelling.
   *
   * `DO UPDATE` sets `verdict` and `note` and leaves `created_at` alone, so a rule re-verdicted
   * this morning still carries the day somebody wrote it down — and "edited" would be inventing a
   * fact the daemon does not hold. The text is SQLite's `datetime('now')`, space-separated and not
   * RFC 3339, so it is split and never parsed: a `new Date(…)` would have rendered a locale
   * spelling or `Invalid Date`, and this assertion is what tells the two apart.
   */
  it("captions a rule with the day it was declared, in the daemon's own spelling", async () => {
    open({ shellRules: [shellRule({ prefix: "npm ci" })] });

    const row = await screen.findByRole("listitem", { name: "rule npm ci" });
    expect(within(row).getByText(/^declared 2026-03-14$/)).toBeDefined();
    expect(row.textContent).not.toMatch(/edited|changed|invalid date/i);

    // The whole UTC text is still reachable, on the caption that abbreviated it.
    expect(within(row).getByTitle(`${DECLARED_ON} UTC`)).toBeDefined();
  });

  /**
   * **The trap `Note` exists to close, asserted at the one gesture that walks into it.**
   *
   * `POST .../shell-rules` rewrites `verdict` AND `note` from what it is sent, deliberately and not
   * as an oversight — a note that fell back to the stored one could never be removed. So a page
   * flipping a verdict with only `{prefix, verdict}` gets a 204 and silently discards the
   * justification, which migration 0128 calls the table's only defence against a list nobody can
   * explain six months later.
   */
  it("keeps a rule's justification when its verdict is flipped", async () => {
    const state = open({
      shellRules: [
        shellRule({ prefix: "npm ci", verdict: "allow", note: "the gate installs before it runs" }),
      ],
    });

    const row = await screen.findByRole("listitem", { name: "rule npm ci" });
    fireEvent.click(within(row).getByRole("button", { name: "refuse it instead" }));

    await waitFor(() => expect(state.policyWrites).toHaveLength(1));
    expect(state.policyWrites[0].body).toEqual({
      prefix: "npm ci",
      verdict: "deny",
      // Resent, not omitted and not emptied.
      note: "the gate installs before it runs",
    });
    // And the row read back still carries it, which is the half a recorded request cannot prove.
    expect(state.shellRules[0].note).toBe("the gate installs before it runs");
    expect(state.shellRules[0].created_at).toBe(DECLARED_ON);
  });

  /**
   * A rule that carried no justification is `{ erase: true }` and never `{ write: "" }`.
   *
   * The two would reach the daemon as `null` and `""`, which are different rows: one is "nobody
   * justified this" and the other is a justification somebody left blank.
   */
  it("sends an explicit null when flipping a rule that never had a justification", async () => {
    const state = open({ shellRules: [shellRule({ prefix: "npm ci", note: null })] });

    const row = await screen.findByRole("listitem", { name: "rule npm ci" });
    fireEvent.click(within(row).getByRole("button", { name: "refuse it instead" }));

    await waitFor(() => expect(state.policyWrites).toHaveLength(1));
    expect(state.policyWrites[0].body).toEqual({ prefix: "npm ci", verdict: "deny", note: null });
  });

  /** The same trap at the form, where somebody re-declaring a prefix is about to walk into it. */
  it("warns that re-declaring with an empty note erases the one on the rule", async () => {
    open({ shellRules: [shellRule({ prefix: "npm ci", note: "the gate needs it" })] });

    const box = await screen.findByLabelText("Command prefix");
    // Typed in the spelling the table does NOT hold: identity is the folded prefix, so the form
    // has to fold before it looks or it offers to "create" a rule that already exists.
    fireEvent.change(box, { target: { value: "NPM  CI" } });

    expect(screen.getByText(/is already declared as/i).textContent).toContain("erases");

    fireEvent.click(screen.getByRole("button", { name: "keep its justification" }));
    expect(screen.getByLabelText<HTMLInputElement>("Why it is here").value).toBe(
      "the gate needs it",
    );
  });

  /**
   * **A refusal's own sentence reaches the screen, verbatim.**
   *
   * `unmatchable_prefix` names the prefix, says why an `allow` of that shape would be stored and
   * never fire, and tells the owner the same prefix declared as a `deny` WOULD be enforced. No
   * sentence this page could write would be better, so it writes none — and the test is what stops
   * somebody adding one later.
   */
  it("puts the núcleo's own words on screen when a prefix could never be allowed", async () => {
    const detail =
      "`ls | sh` can never be allowed: `classify_segment` guards `rules.allows` with the same " +
      "shape check this just failed, so the permission would be stored and never fire. Declared " +
      "as a `deny` it would be enforced.";
    open({ policyRefusal: { status: 422, code: "unmatchable_prefix", detail } });

    const box = await screen.findByLabelText("Command prefix");
    fireEvent.change(box, { target: { value: "ls | sh" } });
    fireEvent.click(screen.getByRole("button", { name: "allow it here" }));

    const note = await screen.findByText(detail);
    expect(note).toBeDefined();
    // The refusal's name travels with it: it is the string that survives rewording, and it is what
    // somebody quotes when the sentence does not explain enough.
    expect(screen.getByText("unmatchable_prefix")).toBeDefined();
  });

  /**
   * The stop's 423, which arrives with no prose at all.
   *
   * `declaration_halted` refuses with the bare name, so the page owes the sentence — and the one
   * that matters is the half the shared floor cannot say: the stop refuses an `allow` and never a
   * `deny`, because a stop that stopped somebody narrowing autonomy would be holding the door open
   * on the way out.
   */
  it("says a refusal would still land when the stop blocks an allow", async () => {
    open({ policyRefusal: { status: 423, code: "kill_switch", detail: "" } });

    const box = await screen.findByLabelText("Command prefix");
    fireEvent.change(box, { target: { value: "bash scripts/gates.sh" } });
    fireEvent.click(screen.getByRole("button", { name: "allow it here" }));

    const note = await screen.findByRole("status");
    expect(note.textContent).toContain("kill_switch");
    expect(note.textContent).toMatch(/is not blocked/i);
    expect(note.textContent).toContain("narrowing");
  });

  it("groups the rows by verdict rather than expecting two lists", async () => {
    open({
      shellRules: [
        shellRule({ prefix: "bash scripts/gates.sh", verdict: "allow" }),
        shellRule({ prefix: "git push", verdict: "deny" }),
      ],
    });

    const allowed = await screen.findByRole("listitem", { name: "rule bash scripts/gates.sh" });
    const refused = screen.getByRole("listitem", { name: "rule git push" });

    // Each in the list its verdict puts it in, which is what a page grouping a flat array has to
    // get right and the only thing that could silently be got wrong.
    expect(allowed.closest("div")?.textContent).toContain("Allowed");
    expect(refused.closest("div")?.textContent).toContain("Refused");
  });
});

describe("where the work lands", () => {
  /**
   * The integration branch needs no row, so it gets no control.
   *
   * An empty table means "nowhere but the usual place" and not "nowhere". A close button beside it
   * would be a control whose only possible answer is `no_such_target` — the same reading section 2
   * takes about an operation outside the ceiling, one section up.
   */
  it("draws the integration branch as always admissible, with nothing to close", async () => {
    open({ landTargets: ["release/next"] });

    const always = await screen.findByRole("listitem", {
      name: "land target the integration branch",
    });
    expect(always.textContent).toContain("master");
    expect(always.textContent).toContain("always admissible");
    expect(within(always).queryByRole("button")).toBeNull();

    // A declared target IS closable, from the same list, which is what makes the absence above a
    // decision rather than an omission.
    const declared = screen.getByRole("listitem", { name: "land target release/next" });
    expect(within(declared).getByRole("button", { name: "close" })).toBeDefined();
  });

  it("opens a target and reads it back", async () => {
    const state = open();

    const box = await screen.findByLabelText("Another landing target");
    fireEvent.change(box, { target: { value: "release/next" } });
    fireEvent.click(screen.getByRole("button", { name: "open it" }));

    await waitFor(() => expect(state.landTargets).toEqual(["release/next"]));
  });
});

describe("the remote", () => {
  /**
   * §5.1's rule read in the direction it also points: *"um painel vazio é indistinguível de um
   * repositório sem PRs"*. Nothing serves this section yet — a typed read carries the repository it
   * reads and no route says which repository a project is — so the honest answer is the explanation
   * and never a panel that stays empty for a reason nobody can see.
   */
  it("explains itself rather than drawing an empty panel", async () => {
    open();

    const remote = await screen.findByRole("region", { name: "The remote" });
    expect(remote.textContent).toContain("not wired yet");

    fireEvent.click(within(remote).getByRole("button", { name: "why?" }));
    const why = remote.textContent ?? "";
    expect(why).toContain("pr_list");
    expect(why).toContain("owner/name");
    expect(why).toMatch(/no route tells this app which repository a project is/i);
  });
});

describe("the page as a whole", () => {
  /**
   * These are a live autonomy control and not a configuration edit.
   *
   * `hooks.rs` reads the shell table per tool call, with no cache and no restart, so a rule written
   * here binds the very next tool call of every in-flight run of this project. A page that read as
   * a preferences pane would be inviting somebody to try something against runs working right now.
   */
  it("says a rule written here binds the next tool call of a run already going", async () => {
    open();

    const said = (await screen.findByText(/no cache and no restart/i)).textContent ?? "";
    expect(said).toContain("very next tool call");
    expect(said).toContain("nucleos");
  });

  it("draws the four sections in the order the design fixes", async () => {
    open();

    await screen.findByRole("region", { name: "The remote" });
    expect(screen.getAllByRole("region").map((region) => region.getAttribute("aria-label"))).toEqual(
      [
        "The remote",
        "What runs on its own",
        "What the worktrees may run",
        "Where the work lands",
      ],
    );
  });
});
