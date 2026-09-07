import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "./client";
import {
  declaredRule,
  declaredVerdict,
  foldPathPrefix,
  foldPrefix,
  useDeclarableGitOps,
  useDeclarableGithubOps,
  useDeclareGitOp,
  useDeclareGithubOp,
  useDeclareLandTarget,
  useDeclareShellRule,
  useForgetGitOp,
  useForgetGithubOp,
  useForgetLandTarget,
  useForgetShellRule,
  useProjectGitOps,
  useProjectGithubOps,
  useProjectLandTargets,
  useProjectShellRules,
  type DeclarableGitOp,
  type DeclarableOp,
  type ShellRule,
  type IntegrationBranch,
  type ShellRuleDeclaration,
  type Verdict,
} from "./project-policy";

/**
 * The seam is `data/client.ts`, as the harness says: the real hooks, the real cache and the real
 * `ApiRefusal` run, and only the daemon is a fake. Spreading the original is what keeps
 * `ApiRefusal` the class the module under test will actually be caught as — an `instanceof` here
 * matches an error a hook threw.
 */
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

type Refusal = InstanceType<typeof ApiRefusal>;

/**
 * What `GET /github/declarable-ops` serves.
 *
 * A stand-in and not a copy: the real one is derived in `github.rs` by intersecting the built
 * operations with two compiled ceilings, and writing the real set out here would be a second
 * spelling of something the shell is deliberately not allowed to hold — which is the very drift the
 * route exists to end.
 *
 * The `declarable: false` rows are the half worth putting in a fixture. An operation outside the
 * ceilings is not missing: it exists, nothing on any screen can turn it on, and a page has to be
 * able to draw that as a fact.
 */
const CATALOGUE: DeclarableOp[] = [
  { kind: "pr_view", half: "read", declarable: true },
  { kind: "run_list", half: "read", declarable: true },
  { kind: "workflow_list", half: "read", declarable: true },
  { kind: "run_logs", half: "read", declarable: false },
  { kind: "pr_comment", half: "action", declarable: true },
  { kind: "api_read", half: "action", declarable: false },
];

/**
 * The operations this fake's POST accepts, DERIVED from the catalogue above rather than written
 * twice — the fake may be a stand-in for the daemon, but it must not be able to disagree with
 * itself about which of the two answers is right.
 */
const ADMITTED = CATALOGUE.filter((operation) => operation.declarable).map(
  (operation) => operation.kind,
);

/**
 * The compiled git operations this fake's shared queue knows how to build.
 *
 * All six are writes with the same queue semantics, so there is deliberately no `half`: importing
 * the GitHub split here would make the fixture promise a distinction the daemon does not make.
 */
const GIT_CATALOGUE: DeclarableGitOp[] = [
  { kind: "merge", declarable: true },
  { kind: "push", declarable: true },
  { kind: "tag", declarable: true },
  { kind: "fetch", declarable: true },
  { kind: "rebase", declarable: true },
  { kind: "branch-delete", declarable: true },
];

/** The day a rule the fixture declares was written down. Any day that is not today will do. */
const DECLARED_ON = "2026-03-14 09:41:00";

/**
 * PURE: the identity of a rule, which is the TOOL and the folded prefix and never the prefix alone.
 *
 * The daemon's unique index is `(project_id, tool, prefix)`, so a project may hold `deny
 * migrations` — a command — and `deny Edit migrations` — a directory — at the same time. A fake
 * keyed on the prefix would let one of them overwrite the other and answer 204 about it, which
 * would make every assertion written above it vacuous.
 *
 * The separator is a NUL because no tool and no prefix can contain one. Sorted, these keys come out
 * in the daemon's own `ORDER BY tool, prefix`: `\u0000` precedes every letter, so the command rules
 * lead, and `Edit` precedes `Write`.
 *
 * WHICH fold follows the tool, exactly as `project_policy` chooses between `fold_prefix` and
 * `fold_path_prefix`: a command loses its case and a path keeps it.
 */
function ruleKey(tool: string | null, prefix: string): string {
  return `${tool ?? ""}\u0000${tool === null ? foldPrefix(prefix) : foldPathPrefix(prefix)}`;
}

/**
 * The three tables, behind the nine routes.
 *
 * Stateful, because the assertions that matter are about a write and then a read: a POST answering
 * 204 proves only that the request was well formed. What is under test is whether the list the
 * shell shows afterwards is the list the daemon is now enforcing.
 */
function fakeDaemon(project = "alpha") {
  /** Keyed by {@link ruleKey}, holding the whole row the GET serves. */
  const rules = new Map<string, ShellRule>();
  const ops = new Set<string>();
  const gitOps = new Set<string>();
  const targets = new Set<string>();
  /*
    Where a landing goes with no argument. `declared`, from `autopilot_state.integration_branch` —
    and pointedly NOT the branch the main checkout is standing on, which is the read `land.rs` exists
    to have replaced.
  */
  let integration: IntegrationBranch = { state: "declared", branch: "master" };
  const sent: { path: string; method: string; body: Record<string, unknown> | null }[] = [];

  async function call(path: string, init?: RequestInit): Promise<unknown> {
    const method = init?.method ?? "GET";
    const body =
      typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null;
    sent.push({ path, method, body });

    // The one route here that names no project: the declarable set is compiled into the daemon, so
    // it is the same answer whichever project a page is showing.
    if (path === "/github/declarable-ops") return CATALOGUE;
    if (path === "/vcs/declarable-ops") return GIT_CATALOGUE;

    const route = /^\/projects\/([^/]+)\/(shell-rules|github-ops|git-ops|land-targets)$/.exec(path);
    if (route === null) throw new Error(`the fake daemon has no route for ${method} ${path}`);
    const [, id, table] = route;

    // The GETs ask the roster nothing — an unknown project reads as a project with nothing
    // declared — while every write 404s by name. The asymmetry is the daemon's and not a shortcut
    // here: `get_project_shell_rules` has no roster check, `post_project_shell_rule` does.
    if (method !== "GET" && id !== project) {
      throw new ApiRefusal(404, "no_such_project", `this daemon has no project called \`${id}\``);
    }
    if (id !== project) return [];

    if (table === "shell-rules") {
      if (method === "GET") {
        // `ORDER BY tool, prefix`, over the folded spelling, which is the only one stored. One row
        // per rule, carrying its own verdict and its own tool — see `ShellRule` for why that and
        // not two lists, and `ruleKey` for why sorting the keys is that ORDER BY.
        return [...rules.keys()].sort().map((key) => rules.get(key) as ShellRule);
      }
      // Absent means a rule about a COMMAND, which is the reading every request written before the
      // field existed had — and `#[serde(default)]` on both request shapes is the daemon saying so.
      const tool = (body?.tool as string | null | undefined) ?? null;
      const prefix = tool === null ? foldPrefix(String(body?.prefix ?? "")) : foldPathPrefix(String(body?.prefix ?? ""));
      const key = ruleKey(tool, String(body?.prefix ?? ""));
      if (method === "POST") {
        // `note = excluded.note`, and not a merge. Whatever arrived is now the note, `null`
        // included — which is the trap `Note` exists to make somebody choose out loud. `created_at`
        // is not in the `DO UPDATE` at all, so a redeclaration keeps the day the rule was first
        // written down.
        rules.set(key, {
          prefix,
          tool,
          verdict: body?.verdict as Verdict,
          note: (body?.note as string | null | undefined) ?? null,
          created_at: rules.get(key)?.created_at ?? DECLARED_ON,
        });
        return undefined;
      }
      if (!rules.delete(key)) {
        // The tool is in the sentence when there is one, because "there is no rule for
        // `migrations`" can be false on the very screen it is read on: the other one is still
        // listed. The command half names the FOLDED spelling, which is what missed.
        throw new ApiRefusal(
          404,
          "no_such_rule",
          tool === null
            ? `${id} has declared no rule for \`${prefix}\``
            : `${id} has declared no \`${tool}\` rule for \`${prefix}\``,
        );
      }
      return undefined;
    }

    if (table === "github-ops") {
      if (method === "GET") return [...ops].sort();
      const opKind = String(body?.op_kind ?? "");
      if (method === "POST") {
        if (!ADMITTED.includes(opKind)) {
          throw new ApiRefusal(
            422,
            "undeclarable_op",
            `\`${opKind}\` is not an operation a project may declare; the ceilings admit: ` +
              ADMITTED.join(", "),
          );
        }
        ops.add(opKind);
        return undefined;
      }
      if (!ops.delete(opKind)) {
        throw new ApiRefusal(404, "no_such_op", `${id} has not declared \`${opKind}\``);
      }
      return undefined;
    }

    if (table === "git-ops") {
      if (method === "GET") return [...gitOps].sort();
      const opKind = String(body?.op_kind ?? "");
      if (method === "POST") {
        if (!GIT_CATALOGUE.some((operation) => operation.kind === opKind)) {
          throw new ApiRefusal(
            422,
            "undeclarable_op",
            `\`${opKind}\` is not a git operation this queue can build`,
          );
        }
        gitOps.add(opKind);
        return undefined;
      }
      if (!gitOps.delete(opKind)) {
        throw new ApiRefusal(404, "no_such_op", `${id} has not declared \`${opKind}\``);
      }
      return undefined;
    }

    // Two halves, because the route answers both: where a landing goes by default, and the extra
    // places it may be sent. The default is never in `targets` — it is admissible with no row, so a
    // fake that listed it there would make it look closeable.
    if (method === "GET") return { integration, targets: [...targets].sort() };
    const branch = String(body?.branch ?? "");
    if (method === "POST") {
      targets.add(branch);
      return undefined;
    }
    if (!targets.delete(branch)) {
      throw new ApiRefusal(
        404,
        "no_such_target",
        `${id} has not declared \`${branch}\` as a landing target`,
      );
    }
    return undefined;
  }

  return {
    call,
    sent,
    declareRule: (
      prefix: string,
      verdict: Verdict,
      note: string | null = null,
      tool: string | null = null,
    ) => {
      rules.set(ruleKey(tool, prefix), {
        prefix: tool === null ? foldPrefix(prefix) : foldPathPrefix(prefix),
        tool,
        verdict,
        note,
        created_at: DECLARED_ON,
      });
    },
    declareOp: (opKind: string) => ops.add(opKind),
    declareGitOp: (opKind: string) => gitOps.add(opKind),
    declareTarget: (branch: string) => targets.add(branch),
    setIntegration: (arm: IntegrationBranch) => {
      integration = arm;
    },
  };
}

/** One row as the route serves it, for the PURE tests, which have no fake daemon to ask. */
function shellRow(overrides: Partial<ShellRule> = {}): ShellRule {
  return { prefix: "npm ci", tool: null, verdict: "allow", note: null, created_at: DECLARED_ON, ...overrides };
}

/** A cache from the app's own factory, so the retry policy under test is the app's policy. */
function mount() {
  const queryClient = createAppQueryClient();
  return ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client: queryClient }, children);
}

let fake: ReturnType<typeof fakeDaemon>;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue("token-abc");
  fake = fakeDaemon();
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(fake.call);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("the three reads", () => {
  /**
   * **One row per rule, each carrying its own verdict**, because that is what the route serves —
   * and because a rule handed to a form has to keep the two fields the POST will rewrite. The
   * allow/deny split is a filter over `verdict`, and the two sides are still not mirror images: an
   * `allow` had to pass a shape guard to be stored and a `deny` did not.
   *
   * The prefixes come back FOLDED. The fixture is written in the spelling somebody would type, and
   * what the hook hands over is the spelling the classifier enforces.
   */
  it("serves one row per rule, in the spelling the núcleo stores", async () => {
    fake.declareRule("Remove-Item  -Recurse", "deny", "never from a worktree");
    fake.declareRule("npm ci", "allow");

    const { result } = renderHook(() => useProjectShellRules("alpha"), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data).toEqual([
      { prefix: "npm ci", tool: null, verdict: "allow", note: null, created_at: DECLARED_ON },
      {
        prefix: "remove-item -recurse",
        tool: null,
        verdict: "deny",
        note: "never from a worktree",
        created_at: DECLARED_ON,
      },
    ]);
    // And the split a page draws is a filter, not a shape the route had to hand it pre-sorted.
    expect(result.current.data?.filter((rule) => rule.verdict === "deny")).toHaveLength(1);
  });

  it("reads the github ops as a flat list of operation names", async () => {
    fake.declareOp("run_list");
    fake.declareOp("pr_view");

    const { result } = renderHook(() => useProjectGithubOps("alpha"), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    // Names, not `gh` command lines — a name is what tells `run_status` from `run_logs`.
    expect(result.current.data).toEqual(["pr_view", "run_list"]);
  });

  it("reads the declared git ops as a flat list of queue operation names", async () => {
    fake.declareGitOp("push");
    fake.declareGitOp("merge");

    const { result } = renderHook(() => useProjectGitOps("alpha"), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    // These are queue operation kinds, not command lines: the route records `push`, not every
    // spelling of `git push` the parser could recognize.
    expect(result.current.data).toEqual(["merge", "push"]);
  });

  /** Empty is a real answer: it means "nowhere but the integration branch", which needs no row. */
  it("reads the land targets, and an empty list is not an absent one", async () => {
    const { result } = renderHook(() => useProjectLandTargets("alpha"), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data?.targets).toEqual([]);
  });

  /**
   * **The default comes from the route that owns landings, and it is not in `targets`.**
   *
   * The route used to serve the extra targets alone, and the page filled the default in from
   * `GET /projects/{id}/branches` — `inspect::Branches::integration`, the branch the main checkout
   * is *parked on*. That field's own doc in the núcleo says the design creating `land.rs` killed
   * that read *"precisely because a checkout parked on the wrong branch silently redirected every
   * landing"*, so the page named a branch `--land` refuses while the admissible one appeared
   * nowhere.
   *
   * Kept out of `targets` deliberately: it is admissible with no row, and a caller that found it in
   * the list would offer to close a row that does not exist.
   */
  it("reads where a landing goes by default, apart from the targets it may name", async () => {
    fake.declareTarget("release/next");

    const { result } = renderHook(() => useProjectLandTargets("alpha"), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data?.integration).toEqual({ state: "declared", branch: "master" });
    expect(result.current.data?.targets).toEqual(["release/next"]);
  });

  /**
   * The three arms where naming a branch would not mean it is admissible.
   *
   * *Until the value is real the caption must not assert admissibility* — so the data layer has to
   * carry which of the four cases it is, and not a `string | null` that flattens three of them into
   * the same nothing.
   */
  it("keeps apart the reasons a landing has no admissible default", async () => {
    for (const arm of [
      { state: "stale", branch: "gone" } as const,
      { state: "derived", branch: "master" } as const,
      { state: "unknown", why: "alpha has no folder on this machine" } as const,
    ]) {
      fake.setIntegration(arm);
      const { result } = renderHook(() => useProjectLandTargets("alpha"), { wrapper: mount() });
      await waitFor(() => expect(result.current.isSuccess).toBe(true));
      expect(result.current.data?.integration).toEqual(arm);
    }
  });

  it("asks nothing at all until there is a project to ask about", () => {
    const { result } = renderHook(() => useProjectShellRules(null), { wrapper: mount() });

    expect(result.current.fetchStatus).toBe("idle");
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });
});

describe("declaring", () => {
  /**
   * The whole point of the invalidation. A 204 says the request was well formed; what an owner
   * needs to see is the list the daemon is now enforcing — without a reload, and without a poll,
   * because these lists deliberately have none. The write is the only thing that can refresh them.
   */
  it("shows a declared rule on the list, folded, without being asked twice", async () => {
    const { result } = renderHook(
      () => ({ rules: useProjectShellRules("alpha"), declare: useDeclareShellRule() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.rules.isSuccess).toBe(true));
    expect(result.current.rules.data).toEqual([]);

    await act(async () => {
      await result.current.declare.mutateAsync({
        projectId: "alpha",
        prefix: "Cargo  Fmt",
        tool: null,
        verdict: "allow",
        note: { write: "formatting cannot break anything" },
      });
    });

    await waitFor(() =>
      expect(result.current.rules.data?.map((rule) => rule.prefix)).toEqual(["cargo fmt"]),
    );
  });

  /**
   * `deny_unknown_fields` on all four request bodies turns a stray field into a 422 rather than a
   * silent drop, so the body is built rather than spread out of the hook's input — `projectId` is
   * addressing, not a field.
   */
  it("sends the route's four fields and nothing else", async () => {
    const { result } = renderHook(() => useDeclareShellRule(), { wrapper: mount() });

    await act(async () => {
      await result.current.mutateAsync({
        projectId: "alpha",
        prefix: "npm ci",
        tool: null,
        verdict: "allow",
        note: { write: "why" },
      });
    });

    const post = fake.sent.find((call) => call.method === "POST");
    expect(post?.path).toBe("/projects/alpha/shell-rules");
    expect(Object.keys(post?.body ?? {}).sort()).toEqual(["note", "prefix", "tool", "verdict"]);
    // `null` is SENT and not left out. The route reads the two the same way, so this costs nothing
    // on the wire — what it buys is a caller who had to decide which kind of rule they meant.
    expect(post?.body?.tool).toBeNull();
  });

  /**
   * **The tool travels, and the case of a path travels with it.**
   *
   * `declare_shell_rule` folds a path through `fold_path_prefix`, which does not lower-case,
   * because whether a path's case matters is the filesystem's question and the núcleo answers it at
   * comparison time. A declaration that arrived lower-cased would have thrown that away before the
   * daemon ever saw it — and on a case-sensitive filesystem a `deny` that cannot match is an allow.
   */
  it("declares a rule about a tool, and the path keeps the case it was typed in", async () => {
    const { result } = renderHook(
      () => ({ rules: useProjectShellRules("alpha"), declare: useDeclareShellRule() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.rules.isSuccess).toBe(true));

    await act(async () => {
      await result.current.declare.mutateAsync({
        projectId: "alpha",
        prefix: "  Core\\Migrations/ ",
        tool: "Edit",
        verdict: "deny",
        note: { write: "sqlx checksums a migration that has already been applied" },
      });
    });

    const post = fake.sent.find((call) => call.method === "POST");
    expect(post?.body?.tool).toBe("Edit");
    expect(post?.body?.prefix).toBe("  Core\\Migrations/ ");

    await waitFor(() =>
      expect(result.current.rules.data).toEqual([
        {
          prefix: "Core/Migrations",
          tool: "Edit",
          verdict: "deny",
          note: "sqlx checksums a migration that has already been applied",
          created_at: DECLARED_ON,
        },
      ]),
    );
  });

  it("declares a github op and a landing target for a branch that does not exist yet", async () => {
    const { result } = renderHook(
      () => ({
        ops: useProjectGithubOps("alpha"),
        targets: useProjectLandTargets("alpha"),
        declareOp: useDeclareGithubOp(),
        declareTarget: useDeclareLandTarget(),
      }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.ops.isSuccess).toBe(true));

    await act(async () => {
      await result.current.declareOp.mutateAsync({ projectId: "alpha", opKind: "run_list" });
      // Deliberately a branch nothing has created. Existence is `land::resolve_target`'s question,
      // asked at the moment of landing — the branch is often made by the run that lands into it.
      await result.current.declareTarget.mutateAsync({
        projectId: "alpha",
        branch: "release/next",
      });
    });

    await waitFor(() => expect(result.current.ops.data).toEqual(["run_list"]));
    expect(result.current.targets.data?.targets).toEqual(["release/next"]);
  });

  it("declares a git op and refreshes the project's raw declaration list", async () => {
    const { result } = renderHook(
      () => ({ ops: useProjectGitOps("alpha"), declare: useDeclareGitOp() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.ops.isSuccess).toBe(true));

    await act(async () => {
      await result.current.declare.mutateAsync({ projectId: "alpha", opKind: "push" });
    });

    await waitFor(() => expect(result.current.ops.data).toEqual(["push"]));
    expect(fake.sent.find((call) => call.method === "POST")?.body).toEqual({ op_kind: "push" });
    expect(fake.sent.find((call) => call.method === "POST")?.path).toBe(
      "/projects/alpha/git-ops",
    );
  });
});

describe("withdrawing", () => {
  /**
   * **The prefix travels in the body**, which is the shape a caller is most likely to get wrong:
   * every other `forget` in this data layer names its subject in the path. A prefix carries spaces
   * and slashes and is not a safe path segment, so the route takes it in the body — and its two
   * siblings follow rather than splitting one shape three ways.
   */
  it("takes the rule away and names it in the body rather than in the path", async () => {
    fake.declareRule("npm ci", "allow");

    const { result } = renderHook(
      () => ({ rules: useProjectShellRules("alpha"), forget: useForgetShellRule() }),
      { wrapper: mount() },
    );
    await waitFor(() =>
      expect(result.current.rules.data?.map((rule) => rule.prefix)).toEqual(["npm ci"]),
    );

    await act(async () => {
      await result.current.forget.mutateAsync({ projectId: "alpha", prefix: "NPM  CI", tool: null });
    });

    await waitFor(() => expect(result.current.rules.data).toEqual([]));

    const remove = fake.sent.find((call) => call.method === "DELETE");
    expect(remove?.path).toBe("/projects/alpha/shell-rules");
    // The typed spelling goes over the wire and the daemon folds it — the shell does not fold on
    // the way out, because a second place that computes the key is a second place it can drift.
    expect(remove?.body).toEqual({ prefix: "NPM  CI", tool: null });
  });

  /**
   * **A prefix alone no longer names a rule, and the DELETE body is where that is felt.**
   *
   * The daemon's `WHERE` matches `(project_id, tool, prefix)`. A project may hold `deny migrations`
   * as a command and `deny Edit migrations` as a directory at once, so a withdrawal that left the
   * tool out would take the command rule while the write rule stayed on screen — the failure whose
   * only symptom is "I deleted it and it is still there".
   */
  it("names the tool as well as the prefix, so the other rule of that name survives", async () => {
    fake.declareRule("migrations", "deny");
    fake.declareRule("migrations", "deny", "nothing writes a migration by hand", "Edit");

    const { result } = renderHook(
      () => ({ rules: useProjectShellRules("alpha"), forget: useForgetShellRule() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.rules.data).toHaveLength(2));

    await act(async () => {
      await result.current.forget.mutateAsync({
        projectId: "alpha",
        prefix: "migrations",
        tool: null,
      });
    });

    await waitFor(() =>
      expect(result.current.rules.data).toEqual([
        {
          prefix: "migrations",
          tool: "Edit",
          verdict: "deny",
          note: "nothing writes a migration by hand",
          created_at: DECLARED_ON,
        },
      ]),
    );
    expect(fake.sent.find((call) => call.method === "DELETE")?.body).toEqual({
      prefix: "migrations",
      tool: null,
    });

    // And named, the write rule goes too — a rule about a tool is withdrawable, not permanent.
    await act(async () => {
      await result.current.forget.mutateAsync({
        projectId: "alpha",
        prefix: "migrations",
        tool: "Edit",
      });
    });
    await waitFor(() => expect(result.current.rules.data).toEqual([]));
  });

  it("closes a landing target, in the body too", async () => {
    fake.declareTarget("release/next");

    const { result } = renderHook(
      () => ({ targets: useProjectLandTargets("alpha"), forget: useForgetLandTarget() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.targets.data?.targets).toEqual(["release/next"]));

    await act(async () => {
      await result.current.forget.mutateAsync({ projectId: "alpha", branch: "release/next" });
    });

    await waitFor(() => expect(result.current.targets.data?.targets).toEqual([]));
    expect(fake.sent.find((call) => call.method === "DELETE")?.body).toEqual({
      branch: "release/next",
    });
  });

  it("withdraws a git op by kind in the request body", async () => {
    fake.declareGitOp("push");

    const { result } = renderHook(
      () => ({ ops: useProjectGitOps("alpha"), forget: useForgetGitOp() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.ops.data).toEqual(["push"]));

    await act(async () => {
      await result.current.forget.mutateAsync({ projectId: "alpha", opKind: "push" });
    });

    await waitFor(() => expect(result.current.ops.data).toEqual([]));
    const remove = fake.sent.find((call) => call.method === "DELETE");
    expect(remove?.path).toBe("/projects/alpha/git-ops");
    expect(remove?.body).toEqual({ op_kind: "push" });
  });
});

describe("a refusal reaches the caller with its sentence", () => {
  /**
   * **The `detail` is the whole argument for these routes refusing by name.** `undeclarable_op`
   * alone says "no"; its detail lists the operations the ceilings admit, and that list appears
   * nowhere else on the wire — no route serves it. A data layer that collapsed this into "request
   * failed" would leave the page with no way to tell an owner what they may declare.
   */
  it("keeps the name and the sentence of a refusal the daemon named", async () => {
    const { result } = renderHook(() => useDeclareGithubOp(), { wrapper: mount() });

    let refused: unknown;
    await act(async () => {
      refused = await result.current
        .mutateAsync({ projectId: "alpha", opKind: "api_read" })
        .catch((error: unknown) => error);
    });

    expect(refused).toBeInstanceOf(ApiRefusal);
    const refusal = refused as Refusal;
    expect(refusal.code).toBe("undeclarable_op");
    expect(refusal.detail).toContain("api_read");
    expect(refusal.detail).toContain("the ceilings admit: pr_view, run_list, workflow_list");
    // And the mutation is holding that same value, not a generic Error: a page reads `error` off
    // the hook rather than off a rejected promise it never had.
    await waitFor(() => expect(result.current.error).toBe(refused));
  });

  it("names the folded prefix when withdrawing a rule that was never declared", async () => {
    const { result } = renderHook(() => useForgetShellRule(), { wrapper: mount() });

    let refused: unknown;
    await act(async () => {
      refused = await result.current
        .mutateAsync({ projectId: "alpha", prefix: "Remove-Item", tool: null })
        .catch((error: unknown) => error);
    });

    const refusal = refused as Refusal;
    expect(refusal.code).toBe("no_such_rule");
    // The folded spelling, because that is the identity that missed. Echoing what was typed would
    // send somebody looking for a rule under a name the table has never held.
    expect(refusal.detail).toContain("`remove-item`");
  });

  /**
   * **The second shape, and the one that is not these routes' doing.** Everything the nine handlers
   * refuse is `{refusal, detail}` JSON; a body axum cannot deserialize at all — a misspelled field,
   * now that all four structs carry `deny_unknown_fields` — is rejected by the extractor before any
   * handler runs, and arrives as axum's own `text/plain`. Both have to reach a page as something it
   * can put on screen.
   *
   * The real `apiFetch` runs here, over a stubbed `fetch`, because turning the two wire shapes into
   * one `ApiRefusal` is the client's job and this is the test that proves a hook carries the prose
   * half of it through. The code is derived from the status — only the route knows what its 422
   * meant — and the sentence is axum's, verbatim.
   */
  it("carries axum's plain-text rejection through as the sentence it is", async () => {
    const real = await vi.importActual<typeof import("./client")>("./client");
    const prose =
      "Failed to deserialize the JSON body into the target type: unknown field `notes`, " +
      "expected one of `prefix`, `tool`, `verdict`, `note` at line 1 column 52";
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: false,
        status: 422,
        statusText: "Unprocessable Entity",
        text: async () => prose,
        json: async () => {
          throw new SyntaxError("not JSON");
        },
      } as unknown as Response),
    );
    daemon.apiFetch.mockImplementation(real.apiFetch);

    const { result } = renderHook(() => useDeclareShellRule(), { wrapper: mount() });

    let refused: unknown;
    await act(async () => {
      refused = await result.current
        .mutateAsync({
          projectId: "alpha",
          prefix: "npm ci",
          tool: null,
          verdict: "allow",
          note: { write: "why" },
        })
        .catch((error: unknown) => error);
    });

    expect(refused).toBeInstanceOf(ApiRefusal);
    const refusal = refused as Refusal;
    expect(refusal.status).toBe(422);
    expect(refusal.code).toBe("unprocessable");
    expect(refusal.detail).toBe(prose);
  });
});

describe("the note a rule carries", () => {
  /**
   * **The whole loop, and it only closes because the GET carries the note.**
   *
   * `declare_shell_rule` is declarative: `note = excluded.note` overwrites, so a second POST
   * carrying no note writes `NULL` over the justification the rule already had. An editor flipping
   * a verdict has to send the existing note back — and while the GET served two lists of prefixes
   * it had nowhere to read that note from. This test used to reach into the fake's table for
   * exactly that reason.
   *
   * So it asks the daemon, and every value it sends back came out of the answer. Make the GET drop
   * `note` again and there is nothing to resend, which is the failure the old shape made
   * unobservable.
   */
  it("reads a rule's note back and keeps it through a verdict change", async () => {
    fake.declareRule("remove-item", "deny", "nothing here deletes recursively");

    const { result } = renderHook(
      () => ({ rules: useProjectShellRules("alpha"), declare: useDeclareShellRule() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.rules.isSuccess).toBe(true));

    // Found under the spelling somebody would type, which is what a form has to be able to do.
    const showing = declaredRule(result.current.rules.data ?? [], "Remove-Item  ");
    expect(showing?.note).toBe("nothing here deletes recursively");

    await act(async () => {
      await result.current.declare.mutateAsync({
        projectId: "alpha",
        prefix: showing?.prefix ?? "",
        tool: showing?.tool ?? null,
        verdict: "allow",
        note: { write: showing?.note ?? "" },
      });
    });

    await waitFor(() =>
      expect(result.current.rules.data).toEqual([
        {
          prefix: "remove-item",
          tool: null,
          verdict: "allow",
          note: "nothing here deletes recursively",
          created_at: DECLARED_ON,
        },
      ]),
    );
  });

  /**
   * The other half of the same operation, and the reason `Note` is a union rather than an optional
   * string: erasing is a thing somebody may genuinely want, and it must not be what happens to
   * somebody who simply had nothing to say about the note.
   */
  it("erases the justification only when a caller asks for that in so many words", async () => {
    fake.declareRule("remove-item", "deny", "nothing here deletes recursively");

    const { result } = renderHook(
      () => ({ rules: useProjectShellRules("alpha"), declare: useDeclareShellRule() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.rules.isSuccess).toBe(true));

    await act(async () => {
      await result.current.declare.mutateAsync({
        projectId: "alpha",
        prefix: "Remove-Item",
        tool: null,
        verdict: "allow",
        note: { erase: true },
      });
    });

    // `null` on the wire and not a missing field. The route's `Option<String>` reads the two the
    // same way, and sending it says out loud that the absence was a decision.
    const post = fake.sent.find((call) => call.method === "POST");
    expect(post?.body).toEqual({
      prefix: "Remove-Item",
      tool: null,
      verdict: "allow",
      note: null,
    });
    // And the erasure is visible where the note now is: on the row the GET serves.
    await waitFor(() => expect(result.current.rules.data?.[0].note).toBeNull());
  });

  /**
   * **And the compiler is the guard, not this comment.** A declaration with no `note` at all is the
   * shape that silently discards a justification, so it does not typecheck: the `@ts-expect-error`
   * below IS the assertion, and `tsc --noEmit` is what runs it. Make `note` optional again and the
   * line stops being an error, which makes the line itself one. Nothing is sent — the point is that
   * the request can never be built.
   */
  it("will not let a declaration leave the note unmentioned", () => {
    // @ts-expect-error `note` is required: leaving it out is how a stored justification is lost.
    const forgotten: ShellRuleDeclaration = { projectId: "alpha", prefix: "npm ci", verdict: "allow" };

    expect(forgotten.prefix).toBe("npm ci");
  });
});

describe("the spelling a rule is stored under", () => {
  /**
   * The fold is `classifier::normalize_command`, and case is not part of a rule's identity. A form
   * that could not say so would offer to create a rule that exists and then overwrite it.
   */
  it("collapses whitespace and lower-cases, as the núcleo does on the way in", () => {
    expect(foldPrefix("Remove-Item  -Recurse")).toBe("remove-item -recurse");
    expect(foldPrefix("  npm\tci ")).toBe("npm ci");
  });

  /**
   * ASCII only, because `to_ascii_lowercase` is ASCII only. A plain `toLowerCase()` would fold this
   * where the núcleo does not, and the two would disagree about the identity of a rule — quietly,
   * and in the direction where a `deny` matches nothing.
   */
  it("leaves alone what the núcleo leaves alone", () => {
    expect(foldPrefix("İnvoke")).toBe("İnvoke");
  });

  /**
   * **The path fold, and the omission that is the whole of it: it does not lower-case.**
   *
   * A mirror of `project_policy::fold_path_prefix` — trim, `\` as `/`, no trailing `/`. A command
   * is case-insensitive to us, because `Remove-Item` and `remove-item` are one cmdlet whatever the
   * filesystem thinks; a path's case belongs to the FILESYSTEM, and the núcleo answers that question
   * at comparison time in `write_denied_by_project`. A preview that lower-cased would show an owner
   * a path that is not the one being stored — and on a case-sensitive filesystem the stored rule
   * would be a refusal that can never match, which is an allow.
   */
  it("folds a path without touching its case, unlike a command prefix", () => {
    expect(foldPathPrefix("  Core\\Migrations/ ")).toBe("Core/Migrations");
    expect(foldPathPrefix("migrations/")).toBe("migrations");
    expect(foldPathPrefix("docs\\notes")).toBe("docs/notes");
    // The same string through the two folds, which is the difference stated as one line.
    expect(foldPrefix("Core/Migrations")).toBe("core/migrations");
    expect(foldPathPrefix("Core/Migrations")).toBe("Core/Migrations");
  });

  /**
   * A prefix and a tool together are a rule's identity, so a lookup that ignored the tool would
   * answer about the wrong rule — telling a form the `Edit` rule it is about to declare already
   * exists, and offering it the command rule's justification to keep.
   */
  it("tells a rule about a path from a rule about a command of the same name", () => {
    const rules: ShellRule[] = [
      shellRow({ prefix: "migrations", tool: null, verdict: "deny" }),
      shellRow({
        prefix: "migrations",
        tool: "Edit",
        verdict: "deny",
        note: "nothing writes a migration by hand",
      }),
    ];

    expect(declaredRule(rules, "migrations")?.tool).toBeNull();
    expect(declaredRule(rules, "migrations", "Edit")?.note).toBe(
      "nothing writes a migration by hand",
    );
    // A tool nothing was declared under is not the other rule wearing a different name.
    expect(declaredRule(rules, "migrations", "Write")).toBeNull();
    // And the lookup folds by the tool it was given: a path keeps its case, so this misses.
    expect(declaredRule(rules, "MIGRATIONS", "Edit")).toBeNull();
    expect(declaredRule(rules, "MIGRATIONS")?.tool).toBeNull();
  });

  it("finds a rule under the spelling somebody typed, on whichever side it sits", () => {
    const rules: ShellRule[] = [
      shellRow({ prefix: "npm ci", verdict: "allow" }),
      shellRow({
        prefix: "remove-item -recurse",
        verdict: "deny",
        note: "never from a worktree",
      }),
    ];

    expect(declaredVerdict(rules, "NPM  CI")).toBe("allow");
    expect(declaredVerdict(rules, "Remove-Item  -Recurse")).toBe("deny");
    // Undeclared is not a verdict, and a page that read it as one would draw a command nobody has
    // ruled on as permitted.
    expect(declaredVerdict(rules, "cargo build")).toBeNull();

    // **The whole row, because the row is what an edit sends back.** `declaredVerdict` is this with
    // three fields thrown away; anything about to WRITE wants the note, and a lookup that could
    // only answer "deny" would send an editor back to the trap `Note` exists to name.
    expect(declaredRule(rules, "Remove-Item  -Recurse")).toEqual(rules[1]);
    expect(declaredRule(rules, "cargo build")).toBeNull();
  });
});

describe("what a project MAY declare", () => {
  /**
   * **The catalogue, and the operations it says NO to.**
   *
   * `POST /projects/{id}/github-ops` validates against this set, and until the route existed the
   * only place it reached the shell was inside an `undeclarable_op` refusal — so a picker had to
   * hardcode the names and drift from the compiled ceilings in silence, or discover them by sending
   * something invalid.
   *
   * The `declarable: false` rows are asserted by name because they are the reason the route serves
   * every operation rather than the admitted ones alone. `api_read` is outside `ACTION_CEILING`:
   * nothing on any screen can turn it on, so it has to be drawable as a fact and never as a control.
   */
  it("names every operation, and says which of them the ceilings admit", async () => {
    const { result } = renderHook(() => useDeclarableGithubOps(), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data).toEqual(CATALOGUE);

    const outside = result.current.data?.filter((operation) => !operation.declarable);
    expect(outside?.map((operation) => operation.kind)).toEqual(["run_logs", "api_read"]);

    // Which door each goes through, because the two are not the same promise once declared: a
    // declared read binds the very next decision, a declared action is stored and inert.
    const half = (kind: string) =>
      result.current.data?.find((operation) => operation.kind === kind)?.half;
    expect(half("api_read")).toBe("action");
    expect(half("run_list")).toBe("read");
  });

  /** It names no project, so there is nothing to wait for — unlike the three per-project reads. */
  it("asks without being given a project", async () => {
    const { result } = renderHook(() => useDeclarableGithubOps(), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(daemon.apiFetch).toHaveBeenCalledWith("/github/declarable-ops");
  });

  /**
   * **A page needs both lists and they must not be confused.** This one is what MAY be declared and
   * is the same for every project; `useProjectGithubOps` is what one project HAS declared. A picker
   * draws the first and ticks the second.
   */
  it("is not the same list as what one project has declared", async () => {
    fake.declareOp("run_list");

    const { result } = renderHook(
      () => ({ every: useDeclarableGithubOps(), mine: useProjectGithubOps("alpha") }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.mine.isSuccess).toBe(true));

    expect(result.current.mine.data).toEqual(["run_list"]);
    expect(result.current.every.data?.length).toBeGreaterThan(1);
  });

  it("reads the machine-wide git queue catalogue without a project id or a GitHub half", async () => {
    const { result } = renderHook(() => useDeclarableGitOps(), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data).toEqual(GIT_CATALOGUE);
    expect(daemon.apiFetch).toHaveBeenCalledWith("/vcs/declarable-ops");
    expect(result.current.data?.every((operation) => !("half" in operation))).toBe(true);
  });
});

describe("a project this daemon has never heard of", () => {
  /**
   * The GETs ask the roster nothing and every write 404s by name, which is the daemon's own
   * asymmetry. It is worth a test because the two together closed a real hole: a typo'd id used to
   * answer 204 and then serve the rule back through the GET, so the owner's only feedback loop
   * confirmed a rule nothing would ever enforce.
   */
  it("is refused by name on a write, and reads as a project with nothing declared", async () => {
    const { result } = renderHook(
      () => ({ ops: useProjectGithubOps("ghost"), forget: useForgetGithubOp() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.ops.isSuccess).toBe(true));
    expect(result.current.ops.data).toEqual([]);

    let refused: unknown;
    await act(async () => {
      refused = await result.current.forget
        .mutateAsync({ projectId: "ghost", opKind: "run_list" })
        .catch((error: unknown) => error);
    });

    const refusal = refused as Refusal;
    expect(refusal.code).toBe("no_such_project");
    expect(refusal.detail).toContain("ghost");
  });
});
