// §spec alcada-por-projecto
import { describe, expect, it, vi } from "vitest";
import { focusManager } from "@tanstack/react-query";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import {
  DECLARED_ON,
  daemonFetch,
  daemonState,
  readOutcome,
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

  /**
   * **A row the project's table holds, captioned as though it did not.**
   *
   * `GET /projects/{id}/github-ops` serves the table RAW — `try_github_ops` narrows nothing, and the
   * route says why — while `delete_project_github_op` deliberately carries no declarability check:
   * *"withdrawing narrows, and an operation stored before the ceilings moved still has to be
   * removable."* **That sentence was written for this page**, and the page was not honouring it: a
   * declared operation outside the ceilings drew the same "nothing on this machine can turn it on"
   * as an undeclared one, with no control at all, so the owner could neither see the row nor remove
   * it.
   *
   * The authority was never wrong — `Policy::for_project` narrows it away, so the operation does not
   * run. What was wrong is that the page contradicted the table, and offered no way to make them
   * agree.
   */
  it("lets a declaration outside the ceiling be seen and withdrawn", async () => {
    const state = open({ githubOps: ["api_read"] });

    const row = await screen.findByRole("listitem", { name: "operation api_read" });
    expect(row.textContent).toContain("declared here");
    expect(row.textContent).toMatch(/it does not run/i);

    // Still never a checkbox: the ceilings do not admit it, so there is nothing to re-grant.
    expect(within(row).queryByRole("checkbox")).toBeNull();

    fireEvent.click(within(row).getByRole("button", { name: "withdraw" }));
    await waitFor(() => expect(state.githubOps).toEqual([]));
    expect(state.policyWrites[0]).toMatchObject({
      method: "DELETE",
      body: { op_kind: "api_read" },
    });
  });

  /**
   * A kind this build no longer has at all vanished from the page entirely.
   *
   * Not the same case as an operation outside the ceilings: those are still in the catalogue, with a
   * half to file them under. These appear in `mine.data` and in neither half — a renamed operation,
   * one removed between versions — and a page rendering only from the catalogue dropped them
   * silently. A row nobody can see is a row nobody can remove, and it stays in the table for ever.
   */
  it("shows a declaration this build no longer has, with the one gesture that removes it", async () => {
    const state = open({ githubOps: ["an_op_this_build_no_longer_has"] });

    const row = await screen.findByRole("listitem", {
      name: "operation an_op_this_build_no_longer_has",
    });
    fireEvent.click(within(row).getByRole("button", { name: "withdraw" }));

    await waitFor(() => expect(state.githubOps).toEqual([]));
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

    /*
      And the reads half says the same kind of true thing, which unqualified it did not.
      `GithubRuntime::policy_for_project` returns `Policy::empty()` outright when the pillar is off —
      pinned by `a_switched_off_pillar_stays_off_whatever_the_project_declared` — and
      `post_project_github_op` has no `enabled` check, so a declaration can be made on a machine
      where nothing will ever consult it. "Consulted at the next decision" was unconditional and the
      núcleo makes no such promise.
    */
    const reads = screen.getByText(/consulted at the next decision/i);
    expect(reads.textContent).toMatch(/only while the GitHub pillar is on/i);
    expect(reads.textContent).toContain(".ai/github.yaml");
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
      // Carried back as it came. A flip of a COMMAND rule must not quietly become a write rule,
      // nor the other way round: the tool is half the identity of the row being rewritten.
      tool: null,
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
    expect(state.policyWrites[0].body).toEqual({
      prefix: "npm ci",
      tool: null,
      verdict: "deny",
      note: null,
    });
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

  /**
   * **Two rules may share a prefix, and they are two rows.**
   *
   * The núcleo's unique index is `(project_id, tool, prefix)`: `deny migrations` is a command
   * nobody may run here and `deny Edit migrations` is a directory nothing may write into, and a
   * project may hold both. Drawn on the prefix alone they are one name given to two rows — React
   * resolves the duplicate key by drawing one of them, and a screen reader is handed "rule
   * migrations" twice with nothing to choose by.
   *
   * **The second rule is DECLARED here rather than seeded, and that is the point of the test.** A
   * seeded pair only proves the page can render two rows it was handed. Writing one through the
   * form puts the fake daemon's identity rule under test as well, and a fake that identified a rule
   * by its folded prefix alone silently overwrote the other one — which would leave every
   * assertion in this section resting on a list the daemon would never have served.
   */
  it("draws a rule about a tool and a rule about a command of that name as two rows", async () => {
    const state = open({
      shellRules: [
        shellRule({ prefix: "migrations", verdict: "deny", note: "not from a worktree" }),
      ],
    });

    fireEvent.change(await screen.findByLabelText("What this rule is about"), {
      target: { value: "Edit" },
    });
    fireEvent.change(screen.getByLabelText("Path prefix"), { target: { value: "migrations" } });
    fireEvent.change(screen.getByLabelText("Why it is here"), {
      target: { value: "nothing writes a migration by hand" },
    });
    fireEvent.click(screen.getByRole("button", { name: "refuse it here" }));

    await waitFor(() => expect(state.shellRules).toHaveLength(2));

    // Two rows, each naming the whole claim it makes. The write rule is not a rule "about
    // migrations" — it is a rule about `Edit` writing to `migrations`.
    const write = await screen.findByRole("listitem", { name: "rule Edit writing to migrations" });
    const command = screen.getByRole("listitem", { name: "rule migrations" });
    expect(write).not.toBe(command);

    // And the tool is on the row, which is the whole of the visual grammar: a prefix standing
    // alone is a command, a prefix with a tool in front of it is a path.
    expect(write.textContent).toContain("Edit");
    expect(write.textContent).toContain("may not write to");
    expect(command.textContent).not.toContain("may not write to");
  });

  /**
   * **The flip is not offered on a rule about a tool**, because its opposite is the one verdict the
   * route refuses. `post_project_shell_rule` answers `unenforceable_allow` to an `allow` beside a
   * tool — the write chain in `classifier::classify` has no allow side to reach — so a control here
   * would be a request the page knows will fail, and a refusal on screen that says nothing about
   * anything the owner did.
   *
   * Asserted as ABSENT and not as disabled, for §5.2's reason three sections up: a disabled control
   * still says "this is a switch, and it is off". The truth is that a write rule has one verdict.
   */
  it("does not offer to flip a rule about a tool, the way it does for a command", async () => {
    open({
      shellRules: [
        shellRule({ prefix: "migrations", tool: "Edit", verdict: "deny" }),
        shellRule({ prefix: "git push", verdict: "deny" }),
      ],
    });

    const write = await screen.findByRole("listitem", { name: "rule Edit writing to migrations" });
    expect(within(write).queryByRole("button", { name: /instead/ })).toBeNull();
    // Withdrawing it is still offered: a refusal a project can never remove is a different problem.
    expect(within(write).getByRole("button", { name: "forget" })).toBeDefined();

    // And the command rule beside it keeps the gesture, so this is about the KIND of rule and not
    // about the section having lost its controls.
    const command = screen.getByRole("listitem", { name: "rule git push" });
    expect(within(command).getByRole("button", { name: "allow it instead" })).toBeDefined();
  });

  /**
   * Choosing a tool makes `allow` unreachable in the form, and not merely refused on submit.
   *
   * A rule about a tool can only deny. Leaving the button and letting the 422 explain would be
   * teaching the rule by refusal, one owner at a time; taking the control away and saying why is
   * the same fact stated before anybody spends a request on it.
   */
  it("takes the allow away when the rule is about a tool, and says why", async () => {
    open();

    const kind = await screen.findByLabelText("What this rule is about");
    expect(screen.getByRole("button", { name: "allow it here" })).toBeDefined();

    fireEvent.change(kind, { target: { value: "Edit" } });

    expect(screen.queryByRole("button", { name: "allow it here" })).toBeNull();
    expect(screen.getByRole("button", { name: "refuse it here" })).toBeDefined();
    // The reason stands in its place, which is what stops this reading as a missing feature.
    expect(screen.getByText(/can only refuse/i)).toBeDefined();

    // Back to a command, and the widening half returns — the control is about the kind of rule
    // being written and not a switch somebody turned off for the session.
    fireEvent.change(kind, { target: { value: "" } });
    expect(screen.getByRole("button", { name: "allow it here" })).toBeDefined();
  });

  /**
   * The preview shows what will be STORED, and for a path that means keeping its case.
   *
   * `fold_path_prefix` trims, writes `\` as `/` and drops a trailing `/` — and deliberately does
   * not lower-case, because a path's case is the filesystem's business and the núcleo asks it at
   * comparison time. A preview that reused the command fold would show an owner a lower-cased path
   * that is not the one being stored, which is exactly the surprise this preview exists to prevent.
   */
  it("previews a path in the spelling it will be stored under, case and all", async () => {
    open();

    fireEvent.change(await screen.findByLabelText("What this rule is about"), {
      target: { value: "Write" },
    });
    fireEvent.change(screen.getByLabelText("Path prefix"), {
      target: { value: "  Core\\Migrations/ " },
    });

    expect(screen.getByText(/stored and enforced as/i).textContent).toContain("Core/Migrations");
  });

  /**
   * **A refusal this form can no longer provoke still reaches the screen in the daemon's words.**
   *
   * `unenforceable_allow` is the one the tool choice above exists to make unreachable — but the
   * route is the authority and not this page, and a rule declared from anywhere else, or a control
   * somebody adds later, can still earn it. `RefusalNote` prefers a NAMED sentence over the
   * daemon's detail, so page copy for this code would not add to it: it would hide the half that
   * says what would work instead. This is the test that stops somebody writing one.
   */
  it("keeps the núcleo's own sentence about an allow that names a tool", async () => {
    const detail =
      "a rule about a tool can only REFUSE. `Edit` allowed to write to `.ai` would be stored and " +
      "decide nothing: the write chain in `classifier::classify` ends at `read-local`, so there " +
      "is no allow side for it to reach. Declared as a `deny` the same path WOULD be enforced.";
    open({ policyRefusal: { status: 422, code: "unenforceable_allow", detail } });

    fireEvent.change(await screen.findByLabelText("What this rule is about"), {
      target: { value: "Edit" },
    });
    fireEvent.change(screen.getByLabelText("Path prefix"), { target: { value: ".ai" } });
    fireEvent.click(screen.getByRole("button", { name: "refuse it here" }));

    expect(await screen.findByText(detail)).toBeDefined();
    // The name travels with it: it is the string that survives a rewording of the sentence.
    expect(screen.getByText("unenforceable_allow")).toBeDefined();
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

  /**
   * **A served `allow` beside a tool is drawn as the dead letter it is, not as a permission.**
   *
   * The route refuses one at the door with `unenforceable_allow`, so this row cannot be WRITTEN
   * from the page — and it can still be READ from it. `project_policy::declared_shell_rules` serves
   * the table whole on purpose, and a row older than that guard, or written out of band, or landed
   * by a migration, comes back with the rest. The deciding read drops it with a `tracing::warn!`
   * nobody standing at a screen will ever see.
   *
   * Which is exactly why the screen has to say it. Drawn with the fixed "may not write to" phrase,
   * this row sat in the **Allowed** panel wearing a refusal's words: a rule that decides nothing,
   * shown as one in force, in the list of permissions. Seeded and not declared, because declaring
   * it is the thing the núcleo correctly makes impossible — the fixture is the table's row, which
   * is what this page is given.
   */
  it("says nothing enforces a served allow that names a tool, and keeps it withdrawable", async () => {
    open({
      shellRules: [
        shellRule({ prefix: "migrations", tool: "Edit", verdict: "allow", note: null }),
        shellRule({ prefix: "npm ci", verdict: "allow" }),
      ],
    });

    const stranded = await screen.findByRole("listitem", {
      name: "rule Edit writing to migrations",
    });

    // The refusal's words are gone, and the row says what is true of it instead.
    expect(stranded.textContent).not.toContain("may not write to");
    expect(stranded.textContent).toContain("nothing enforces this");
    expect(stranded.textContent).toMatch(/can only refuse/);

    // And it is the owner's to remove — a row that decides nothing and cannot be withdrawn is the
    // same problem one move further on, and filtering it out of the list would be that too.
    expect(within(stranded).getByRole("button", { name: "forget" })).toBeDefined();

    // The ordinary permission beside it is untouched: this is about the KIND of row, and not about
    // the Allowed panel having learned to disclaim everything in it.
    const real = screen.getByRole("listitem", { name: "rule npm ci" });
    expect(real.textContent).not.toContain("nothing enforces this");
  });

  /**
   * **The caption over a list promises only what every row under it keeps.**
   *
   * "Never runs here, and never written to — whatever the compiled lists would have said" sat over
   * a list holding both kinds of rule, and the second half is not a property of the list. A write
   * rule is gated on `classifier::WRITE_TOOLS` — `Edit` and `Write`, nothing else — so it does not
   * stop a `Bash` line redirecting into the same directory, and `NotebookEdit` is outside that list
   * entirely. An owner reading `deny rm -rf` under "and never written to" came away believing the
   * path was closed to writes, which nothing on this page had said.
   *
   * So the guarantee moved onto the row, where the tool is, and the caption keeps what survives
   * across the list. The wording is pinned because it is the whole of the fix: a caption is the one
   * thing on this page that is read as a promise over rows nobody scrolled to.
   */
  it("does not promise over the whole refused list what only a tool row can give", async () => {
    open({
      shellRules: [
        shellRule({ prefix: "rm -rf", verdict: "deny" }),
        shellRule({ prefix: "migrations", tool: "Edit", verdict: "deny" }),
      ],
    });

    const command = await screen.findByRole("listitem", { name: "rule rm -rf" });
    const panel = command.closest("div")?.textContent ?? "";

    expect(panel).toContain("Refused");
    // The over-claim, gone: the list no longer says it about the command prefix beside it.
    expect(panel).not.toContain("and never written to");
    // What is left is true of both rows, and it says which half belongs to which.
    expect(panel).toMatch(/A prefix standing alone never runs/);
    expect(panel).toMatch(/never written to by that tool/);

    // And the write half is still stated where it is true — on the row that carries the tool.
    const write = screen.getByRole("listitem", { name: "rule Edit writing to migrations" });
    expect(write.textContent).toContain("may not write to");
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

  /**
   * **The branch a landing goes to, not the one the checkout is standing on.**
   *
   * This is the defect `land.rs` exists to abolish, reappearing in the page that reports on it. The
   * panel used to fill this line from `GET /projects/{id}/branches` —
   * `inspect::Branches::integration`, which is `current_branch(project_root)`, and whose own doc in
   * the núcleo says the 2026-08-27 design killed that read *"precisely because a checkout parked on
   * the wrong branch silently redirected every landing"*.
   *
   * The case is ordinary: any project whose main clone the owner has checked out onto a feature
   * branch. Both sentences on the panel were then false in both directions — the branch named is
   * refused by name (`parked is not among the landing targets recorded for alpha`), and the branch
   * that IS admissible appeared nowhere.
   *
   * So the fixture makes the two disagree on purpose, and the assertion is a pair: the declared
   * branch is there AND the parked one is absent. Asserting only the first would pass on a panel
   * that showed both.
   */
  it("names the declared integration branch, not the branch the checkout is parked on", async () => {
    open({
      branches: { integration: "parked", branches: [], omitted: 0 },
      landIntegration: { state: "declared", branch: "master" },
    });

    const always = await screen.findByRole("listitem", {
      name: "land target the integration branch",
    });
    expect(always.textContent).toContain("master");
    expect(always.textContent).toContain("always admissible");
    expect(always.textContent).not.toContain("parked");
  });

  /**
   * The three arms where a branch name on screen would be a claim the núcleo refuses.
   *
   * *"Until the value is real the caption must not assert admissibility."* A declared branch whose
   * ref is gone, a default nothing has written down yet, and a project with nothing to derive from
   * are three different sentences — and the first two still name a branch, which is precisely why
   * they cannot borrow the caption the first arm gets.
   */
  it.each([
    [
      { state: "stale", branch: "gone" } as const,
      /nothing lands by default until it is corrected/i,
      "gone",
    ],
    [
      { state: "derived", branch: "master" } as const,
      /has not been written down yet/i,
      "master",
    ],
    [{ state: "unknown", why: "alpha has no folder on this machine" } as const, /no folder/i, null],
  ])("says why a landing has no admissible default when it is %s", async (arm, sentence, named) => {
    open({ landIntegration: arm });

    const always = await screen.findByRole("listitem", {
      name: "land target the integration branch",
    });
    expect(always.textContent).toMatch(sentence);
    // The claim that must not be borrowed. A branch is named in two of these three and captioned
    // admissible in none of them.
    expect(always.textContent).not.toContain("always admissible");
    if (named !== null) expect(always.textContent).toContain(named);
  });
});

describe("the remote", () => {
  /**
   * The section, once whatever it is going to say has settled.
   *
   * Waiting on the absence of "asking" rather than on the presence of any particular sentence,
   * because this section has three reads in flight at once — the mapping and two listings — and a
   * helper that waited for one of them would let a test assert against a panel still loading. The
   * idle button says "ask again", which does not match; the fetching one says "asking GitHub…",
   * which does.
   *
   * Both loading lines are named rather than matching "asking" alone, because the advice this
   * section gives about a missing token ends *"asking again is all this needs"* — a looser guard
   * waits for a sentence that is the settled answer.
   */
  async function remote(): Promise<HTMLElement> {
    const region = await screen.findByRole("region", { name: "The remote" });
    await waitFor(() =>
      expect(region.textContent ?? "").not.toMatch(/asking (github|which repository)/i),
    );
    return region;
  }

  /**
   * The mapping is the fact this section waited on, and this is the assertion that it is used.
   *
   * The repository sent to `POST /github/requests` must be the one the daemon named. A page that
   * worked it out for itself — chaining `GET /projects/detect` and parsing the URL into a slug — is
   * exactly what was refused, and the failure it would cause is invisible on screen: somebody else's
   * pull requests, under this project's name, with nothing anywhere saying so.
   */
  it("reads the two listings against the repository the núcleo named", async () => {
    const state = open();

    const section = await remote();
    expect(section.textContent).toContain("duarte/nucleos");
    // The URL beside the slug, which is what somebody checks the slug against.
    expect(section.textContent).toContain("git@github.com:duarte/nucleos.git");

    await waitFor(() => expect(state.githubReads).toHaveLength(2));
    expect([...state.githubReads].sort((a, b) => a.op.localeCompare(b.op))).toEqual([
      { op: "pr_list", repo: "duarte/nucleos" },
      { op: "run_list", repo: "duarte/nucleos" },
    ]);
  });

  /**
   * **`gh`'s prose is rendered and never parsed**, and the assertion is the tab.
   *
   * `--json` is in the núcleo's `REFUSED_READ_FLAGS` on purpose, and so are `--limit` and `-L`:
   * `github.rs` grades these two reads `ReadsOwn` on the strength of `gh`'s own thirty-row cap,
   * *"not a performance detail, it is half of this grading"*. So there is no structured form of this
   * answer to build a table out of, and a page that wanted one would have to ask for the flag that
   * grading depends on being refused. What is on screen is the CLI's own text, in a `pre`, whitespace
   * intact — asserting the tab survives is asserting nobody split it into cells.
   */
  it("renders what gh printed, as gh printed it", async () => {
    open();

    const section = await remote();
    const printed = section.querySelectorAll("pre");
    expect(printed).toHaveLength(2);
    expect(printed[0].textContent).toBe("#41\tthe queue lands\tfeat/land\tabout 2 hours ago");
    expect(printed[1].textContent).toBe(
      "completed\tsuccess\tCI\tmaster\tpush\t9812345\t1m20s",
    );
  });

  /**
   * **The assertion §5.1 argues for, five times over.**
   *
   * *«Sem token ou sem `gh` encontrado, explicado e não em branco — um painel vazio é indistinguível
   * de um repositório sem PRs»*. None of these five is an error and none of them is the same as any
   * other: a project switched off has had its root cleared, a checkout can be moved, only `active`
   * insists on a repository, a local-only project is a project, and a project on GitLab is not a
   * question this app answers. A mapping that collapsed them would leave this section with one
   * sentence for five different things to go and do.
   *
   * Asserted as the SENTENCE each state produces and not merely as "some text": a shared "no
   * repository" line would satisfy a length check while being exactly the failure this forbids.
   */
  it.each([
    ["no_root", /no folder/i, /clears the root/i],
    ["root_missing", /is not there/i, /moved or deleted/i],
    ["not_a_repository", /not a git repository/i, /only active mode insists/i],
    ["no_remote", /no origin/i, /local-only project is a project/i],
    ["not_github", /not a GitHub repository/i, /github\.com and nothing else/i],
  ])("explains a project whose remote is %s rather than going blank", async (state, says, why) => {
    const daemon = open({
      githubRepo: { state, root: "C:/Projects/thing", remote: "https://gitlab.com/a/b.git" } as never,
    });

    const section = await remote();
    expect(section.textContent).toMatch(says);

    fireEvent.click(within(section).getByRole("button", { name: "why?" }));
    expect(section.textContent).toMatch(why);

    // And nothing was asked of GitHub, because there was nothing to ask it about. A page that sent
    // a read with an empty repository would get a refusal it would then have to explain instead.
    expect(daemon.githubReads).toEqual([]);
  });

  /**
   * No token stored — §5.1 names this case by hand, and it is a 403 nothing else can produce.
   *
   * The daemon's own sentence is the better half here and this page must not replace it: the núcleo
   * says *where* the credential goes, which is more than any copy written in the shell would. So the
   * advice is added to it rather than instead of it.
   */
  it("explains a missing token instead of showing two empty panels", async () => {
    open({
      githubReadRefusal: {
        status: 403,
        code: "forbidden",
        detail: "no github token is stored; run the daemon with --set-github-token",
      },
    });

    const section = await remote();
    expect(section.textContent).toContain("no github token is stored");
    expect(section.textContent).toMatch(/Credential Manager/i);
    // Both panels say it. One that stayed blank while the other explained would be the empty panel
    // this rule forbids, in half the section.
    expect(section.querySelectorAll(".ui-note-refusal")).toHaveLength(2);
    expect(section.querySelectorAll("pre")).toHaveLength(0);
  });

  /**
   * `gh` not found — §5.1's other named case, and the 503 it arrives as covers two faults.
   *
   * A switched-off pillar and a missing CLI are both 503; the núcleo keeps them apart in words and
   * not in the status. So the page shows the daemon's sentence, which says which, and adds advice
   * naming both places to look — rather than sniffing the prose to guess, which is the `switch` over
   * sentences `client.ts` says never to build.
   */
  it("explains a gh that is not on the daemon's PATH", async () => {
    open({
      githubReadRefusal: {
        status: 503,
        code: "unavailable",
        detail: "gh is not on this machine's PATH",
      },
    });

    const section = await remote();
    expect(section.textContent).toContain("gh is not on this machine's PATH");
    expect(section.textContent).toMatch(/\.ai\/github\.yaml/);
  });

  /**
   * A refusal this page has written no advice for still says **what the daemon said**.
   *
   * **An alternation here was the bug hiding the bug.** This assertion used to accept either the
   * daemon's sentence or the shared floor's, so it passed while the page was showing the floor —
   * `RefusalNote` resolves page copy → floor → daemon prose, so handing it an empty map does not
   * fall through to the daemon, it falls through to the FLOOR, which is vaguer by construction. A
   * test that tolerates both outcomes of the thing it is testing is not a test.
   *
   * So the daemon's words are required, and the floor's are refused by name.
   */
  it("keeps the daemon's own sentence when it has no advice to add", async () => {
    open({
      githubReadRefusal: {
        status: 500,
        code: "internal",
        detail: "the github task did not finish",
      },
    });

    const section = await remote();
    expect(section.textContent).toContain("the github task did not finish");
    expect(section.textContent).not.toContain("error of its own handling this");
  });

  /**
   * `gh` ran and GitHub said no — a 200 from the núcleo, and the one failure that is not a refusal.
   *
   * The request was fine and the answer was not: a renamed repository, a token without access to it,
   * a network that is down. `stdout` is empty in that case and `output_tail` is where the CLI said
   * why, so a panel that drew `stdout` would be blank at precisely the moment there is most to say.
   */
  it("shows why gh failed rather than an empty panel", async () => {
    open({
      githubListings: {
        pr_list: readOutcome("pr_list", "", {
          exit_code: 1,
          output_tail: "could not resolve to a Repository with the name 'duarte/nucleos'",
        }),
        run_list: readOutcome("run_list", "completed\tsuccess\tCI\tmaster\tpush\t9812345\t1m20s"),
      },
    });

    const section = await remote();
    expect(section.textContent).toMatch(/gh ran and did not succeed \(exit 1\)/);
    expect(section.textContent).toContain("could not resolve to a Repository");
  });

  /**
   * A repository with nothing to list says so, which is the sentence §5.1 is built out of.
   *
   * *«um painel vazio é indistinguível de um repositório sem PRs»* — read in the direction it also
   * points. This is the case where the panel is legitimately empty, and it is exactly the case that
   * must not be drawn as an empty panel, because then nothing on screen separates it from a read
   * that silently failed.
   */
  it("says a listing was empty rather than looking like one that failed", async () => {
    open({
      githubListings: {
        pr_list: readOutcome("pr_list", "", {
          output_tail: "no open pull requests in duarte/nucleos",
        }),
        run_list: readOutcome("run_list", "completed\tsuccess\tCI\tmaster\tpush\t9812345\t1m20s"),
      },
    });

    const section = await remote();
    expect(section.textContent).toMatch(/answered and listed nothing/i);
    expect(section.textContent).toContain("no open pull requests in duarte/nucleos");
  });

  /** A project the roster has never heard of is the one refusal the mapping makes. */
  it("explains a project the núcleo does not have", async () => {
    open({ githubRepo: null });

    const section = await remote();
    expect(section.textContent).toMatch(/no project by this name/i);
  });

  /**
   * Nothing here is polled — each listing is a subprocess and a network call — so the one gesture in
   * the section is the one that asks again.
   */
  it("asks GitHub again when told to", async () => {
    const state = open();

    const section = await remote();
    await waitFor(() => expect(state.githubReads).toHaveLength(2));

    fireEvent.click(within(section).getByRole("button", { name: "ask again" }));
    await waitFor(() => expect(state.githubReads).toHaveLength(4));
  });
});

describe("the page as a whole", () => {
  /**
   * **§5.1's rule applied to the three sections that actually decide authority.**
   *
   * *«um painel vazio é indistinguível de um repositório sem PRs»* — and a guard written as
   * `data === undefined` cannot tell "still loading" from "refused". With `retry: false` (the house
   * default, argued for at every one of these hooks) the loading line was therefore permanent:
   * three sections that govern what an autonomous run may do sat reading *"Reading this project's
   * rules…"* for ever, with nothing on screen admitting anything had gone wrong.
   *
   * `try_github_ops` and `try_land_targets` were given a `Result` rather than swallowing to an empty
   * list precisely so this could be said out loud, and the page was discarding it.
   *
   * Asserted as the ABSENCE of the loading line and the presence of a refusal, because a section
   * that added an explanation *below* a permanent "Reading…" would still be lying about the state.
   */
  it("says the three governing sections could not be read, rather than reading for ever", async () => {
    // The three declaration READS refuse. The catalogue is machine-wide and answers regardless,
    // which is what makes `mine` the failing half in section 2 — the exact split the page has to
    // get right, since it can draw neither list without both.
    open({ policyReadRefusal: { status: 500, code: "internal", detail: "" } });

    for (const label of [
      "What runs on its own",
      "What the worktrees may run",
      "Where the work lands",
    ]) {
      const section = await screen.findByRole("region", { name: label });
      await waitFor(() => expect(section.textContent ?? "").not.toMatch(/Reading/i));
      expect(section.querySelector(".ui-note-refusal")).not.toBeNull();
    }
  });

  /**
   * Alt-tabbing back into the tray must not spend a pair of GitHub API calls.
   *
   * TanStack v5 refetches on window focus by default, `createAppQueryClient` does not turn it off,
   * and `staleTime` is 0 — so the section that documents itself as *"asked once, never polled"* was
   * running `gh` twice and `git` once on every focus event. That is polling with a different
   * trigger, and it contradicts the page's own claim that the button is the only gesture here.
   */
  /**
   * Ticking a checkbox must not re-spawn `git` for a fact no declaration can change.
   *
   * The declaration writes used to invalidate the `keys.projects.all` PREFIX, which was right while
   * the three lists were the only things under it. `keys.projects.githubRepo` is under it now, and
   * that read runs `git remote get-url` against the project root — so every grant and every
   * withdrawal cost a subprocess to re-answer which repository the project is.
   */
  it("does not re-read the repository when a declaration is written", async () => {
    open();

    const repoReads = () =>
      daemon.apiFetch.mock.calls.filter(([path]) => String(path).endsWith("/github-repo")).length;
    await waitFor(() => expect(repoReads()).toBeGreaterThan(0));
    const before = repoReads();

    const row = await screen.findByRole("listitem", { name: "operation pr_list" });
    fireEvent.click(within(row).getByRole("checkbox"));
    await waitFor(() => expect(screen.getByRole("listitem", { name: "operation pr_list" })).toBeDefined());
    await new Promise((resolve) => setTimeout(resolve, 50));

    expect(repoReads()).toBe(before);
  });

  it("does not re-run gh when the window is focused again", async () => {
    const state = open();

    await waitFor(() => expect(state.githubReads).toHaveLength(2));

    focusManager.setFocused(false);
    focusManager.setFocused(true);

    // A refetch would be scheduled synchronously on focus and settle on the next tick; giving it
    // several is what makes the absence meaningful rather than a race the test happened to win.
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(state.githubReads).toHaveLength(2);
  });

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
