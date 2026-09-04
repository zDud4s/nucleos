// §spec alcada-por-projecto
import { describe, expect, it, vi } from "vitest";
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
   * A refusal this page has written no advice for still says what the daemon said.
   *
   * The floor under every other case. `RefusalNote` never renders "request failed" — page copy, then
   * the shared floor, then the daemon's prose — and a status this section has no advice for must
   * fall through to that rather than to nothing.
   */
  it("explains a refusal it has no advice for", async () => {
    open({
      githubReadRefusal: {
        status: 500,
        code: "internal",
        detail: "the github task did not finish",
      },
    });

    const section = await remote();
    expect(section.textContent).toMatch(/did not finish|error of its own/i);
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
