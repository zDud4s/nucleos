import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "./client";
import {
  declaredVerdict,
  foldPrefix,
  useDeclareGithubOp,
  useDeclareLandTarget,
  useDeclareShellRule,
  useForgetGithubOp,
  useForgetLandTarget,
  useForgetShellRule,
  useProjectGithubOps,
  useProjectLandTargets,
  useProjectShellRules,
  type ShellRuleDeclaration,
  type ShellRules,
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
 * The operations this fake's ceilings admit.
 *
 * A stand-in and not a copy: the real set is derived in `github.rs` by intersecting the built
 * operations with two compiled ceilings, and it reaches the shell only inside the `undeclarable_op`
 * refusal. Writing the real one out here would be a second spelling of a set the shell is
 * deliberately not allowed to hold.
 */
const ADMITTED = ["pr_view", "run_list", "workflow_list"];

interface StoredRule {
  verdict: Verdict;
  /** `null` is a rule with no justification, which is a state the table really has. */
  note: string | null;
}

/**
 * The three tables, behind the nine routes.
 *
 * Stateful, because the assertions that matter are about a write and then a read: a POST answering
 * 204 proves only that the request was well formed. What is under test is whether the list the
 * shell shows afterwards is the list the daemon is now enforcing.
 */
function fakeDaemon(project = "alpha") {
  const rules = new Map<string, StoredRule>();
  const ops = new Set<string>();
  const targets = new Set<string>();
  const sent: { path: string; method: string; body: Record<string, unknown> | null }[] = [];

  async function call(path: string, init?: RequestInit): Promise<unknown> {
    const method = init?.method ?? "GET";
    const body =
      typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null;
    sent.push({ path, method, body });

    const route = /^\/projects\/([^/]+)\/(shell-rules|github-ops|land-targets)$/.exec(path);
    if (route === null) throw new Error(`the fake daemon has no route for ${method} ${path}`);
    const [, id, table] = route;

    // The GETs ask the roster nothing — an unknown project reads as a project with nothing
    // declared — while every write 404s by name. The asymmetry is the daemon's and not a shortcut
    // here: `get_project_shell_rules` has no roster check, `post_project_shell_rule` does.
    if (method !== "GET" && id !== project) {
      throw new ApiRefusal(404, "no_such_project", `this daemon has no project called \`${id}\``);
    }
    if (id !== project) return table === "shell-rules" ? { allow: [], deny: [] } : [];

    if (table === "shell-rules") {
      if (method === "GET") {
        const view: ShellRules = { allow: [], deny: [] };
        // `ORDER BY prefix`, over the folded spelling, which is the only one stored.
        for (const prefix of [...rules.keys()].sort()) {
          const stored = rules.get(prefix);
          if (stored !== undefined) view[stored.verdict].push(prefix);
        }
        return view;
      }
      const prefix = foldPrefix(String(body?.prefix ?? ""));
      if (method === "POST") {
        // `note = excluded.note`, and not a merge. Whatever arrived is now the note, `null`
        // included — which is the trap `Note` exists to make somebody choose out loud.
        rules.set(prefix, {
          verdict: body?.verdict as Verdict,
          note: (body?.note as string | null | undefined) ?? null,
        });
        return undefined;
      }
      if (!rules.delete(prefix)) {
        throw new ApiRefusal(404, "no_such_rule", `${id} has declared no rule for \`${prefix}\``);
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

    if (method === "GET") return [...targets].sort();
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
    /**
     * The note a rule is carrying, read out of the table rather than off the wire.
     *
     * There is no other way to ask: `GET /projects/{id}/shell-rules` serves two lists of prefixes
     * and no notes, so a note is write-only through HTTP. That absence is why the preservation
     * test below has to reach in here, and why `Note` cannot do better than make the choice
     * explicit.
     */
    noteFor: (prefix: string) => rules.get(foldPrefix(prefix))?.note ?? null,
    declareRule: (prefix: string, verdict: Verdict, note: string | null = null) => {
      rules.set(foldPrefix(prefix), { verdict, note });
    },
    declareOp: (opKind: string) => ops.add(opKind),
    declareTarget: (branch: string) => targets.add(branch),
  };
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
   * **Two lists and not one with a verdict on each row**, because that is what the route serves —
   * and because the two are not mirror images: an `allow` had to pass a shape guard to be stored
   * and a `deny` did not.
   *
   * The prefixes come back FOLDED. The fixture is written in the spelling somebody would type, and
   * what the hook hands over is the spelling the classifier enforces.
   */
  it("keeps the allow list and the deny list apart, in the spelling the núcleo stores", async () => {
    fake.declareRule("Remove-Item  -Recurse", "deny");
    fake.declareRule("npm ci", "allow");

    const { result } = renderHook(() => useProjectShellRules("alpha"), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data).toEqual({ allow: ["npm ci"], deny: ["remove-item -recurse"] });
  });

  it("reads the github ops as a flat list of operation names", async () => {
    fake.declareOp("run_list");
    fake.declareOp("pr_view");

    const { result } = renderHook(() => useProjectGithubOps("alpha"), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    // Names, not `gh` command lines — a name is what tells `run_status` from `run_logs`.
    expect(result.current.data).toEqual(["pr_view", "run_list"]);
  });

  /** Empty is a real answer: it means "nowhere but the integration branch", which needs no row. */
  it("reads the land targets, and an empty list is not an absent one", async () => {
    const { result } = renderHook(() => useProjectLandTargets("alpha"), { wrapper: mount() });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(result.current.data).toEqual([]);
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
    expect(result.current.rules.data).toEqual({ allow: [], deny: [] });

    await act(async () => {
      await result.current.declare.mutateAsync({
        projectId: "alpha",
        prefix: "Cargo  Fmt",
        verdict: "allow",
        note: { write: "formatting cannot break anything" },
      });
    });

    await waitFor(() => expect(result.current.rules.data?.allow).toEqual(["cargo fmt"]));
  });

  /**
   * `deny_unknown_fields` on all four request bodies turns a stray field into a 422 rather than a
   * silent drop, so the body is built rather than spread out of the hook's input — `projectId` is
   * addressing, not a field.
   */
  it("sends the route's three fields and nothing else", async () => {
    const { result } = renderHook(() => useDeclareShellRule(), { wrapper: mount() });

    await act(async () => {
      await result.current.mutateAsync({
        projectId: "alpha",
        prefix: "npm ci",
        verdict: "allow",
        note: { write: "why" },
      });
    });

    const post = fake.sent.find((call) => call.method === "POST");
    expect(post?.path).toBe("/projects/alpha/shell-rules");
    expect(Object.keys(post?.body ?? {}).sort()).toEqual(["note", "prefix", "verdict"]);
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
    expect(result.current.targets.data).toEqual(["release/next"]);
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
    await waitFor(() => expect(result.current.rules.data?.allow).toEqual(["npm ci"]));

    await act(async () => {
      await result.current.forget.mutateAsync({ projectId: "alpha", prefix: "NPM  CI" });
    });

    await waitFor(() => expect(result.current.rules.data).toEqual({ allow: [], deny: [] }));

    const remove = fake.sent.find((call) => call.method === "DELETE");
    expect(remove?.path).toBe("/projects/alpha/shell-rules");
    // The typed spelling goes over the wire and the daemon folds it — the shell does not fold on
    // the way out, because a second place that computes the key is a second place it can drift.
    expect(remove?.body).toEqual({ prefix: "NPM  CI" });
  });

  it("closes a landing target, in the body too", async () => {
    fake.declareTarget("release/next");

    const { result } = renderHook(
      () => ({ targets: useProjectLandTargets("alpha"), forget: useForgetLandTarget() }),
      { wrapper: mount() },
    );
    await waitFor(() => expect(result.current.targets.data).toEqual(["release/next"]));

    await act(async () => {
      await result.current.forget.mutateAsync({ projectId: "alpha", branch: "release/next" });
    });

    await waitFor(() => expect(result.current.targets.data).toEqual([]));
    expect(fake.sent.find((call) => call.method === "DELETE")?.body).toEqual({
      branch: "release/next",
    });
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
        .mutateAsync({ projectId: "alpha", prefix: "Remove-Item" })
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
      "expected one of `prefix`, `verdict`, `note` at line 1 column 52";
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
   * **The trap, pinned.** `declare_shell_rule` is declarative: `note = excluded.note` overwrites,
   * so a second POST carrying no note writes `NULL` over the justification the rule already had.
   * An editor flipping a verdict has to send the existing note back with it.
   *
   * The assertion reaches into the fake's table because there is nowhere else to look — the GET
   * serves prefixes and no notes, so a note cannot be read back over HTTP at all.
   */
  it("keeps the justification when a verdict is flipped with the note sent back", async () => {
    fake.declareRule("remove-item", "deny", "nothing here deletes recursively");

    const { result } = renderHook(() => useDeclareShellRule(), { wrapper: mount() });
    await act(async () => {
      await result.current.mutateAsync({
        projectId: "alpha",
        prefix: "Remove-Item",
        verdict: "allow",
        note: { write: "nothing here deletes recursively" },
      });
    });

    expect(fake.noteFor("remove-item")).toBe("nothing here deletes recursively");
  });

  /**
   * The other half of the same operation, and the reason `Note` is a union rather than an optional
   * string: erasing is a thing somebody may genuinely want, and it must not be what happens to
   * somebody who simply had nothing to say about the note.
   */
  it("erases the justification only when a caller asks for that in so many words", async () => {
    fake.declareRule("remove-item", "deny", "nothing here deletes recursively");

    const { result } = renderHook(() => useDeclareShellRule(), { wrapper: mount() });
    await act(async () => {
      await result.current.mutateAsync({
        projectId: "alpha",
        prefix: "Remove-Item",
        verdict: "allow",
        note: { erase: true },
      });
    });

    // `null` on the wire and not a missing field. The route's `Option<String>` reads the two the
    // same way, and sending it says out loud that the absence was a decision.
    const post = fake.sent.find((call) => call.method === "POST");
    expect(post?.body).toEqual({ prefix: "Remove-Item", verdict: "allow", note: null });
    expect(fake.noteFor("remove-item")).toBeNull();
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

  it("finds a rule under the spelling somebody typed, on whichever side it sits", () => {
    const rules: ShellRules = { allow: ["npm ci"], deny: ["remove-item -recurse"] };

    expect(declaredVerdict(rules, "NPM  CI")).toBe("allow");
    expect(declaredVerdict(rules, "Remove-Item  -Recurse")).toBe("deny");
    // Undeclared is not a verdict, and a page that read it as one would draw a command nobody has
    // ruled on as permitted.
    expect(declaredVerdict(rules, "cargo build")).toBeNull();
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
