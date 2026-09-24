import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * What is happening on a project's remote, in the daemon's own words.
 *
 * **Not `project-policy.ts`, and the split is the one that module already draws.** That one holds
 * the three lists a project DECLARES about itself — permissions, never executed. This one runs
 * things: it asks which repository a project is, and then sends `gh` at it. §5.1 is the section
 * above §5.2 on the page for the same reason they are two modules here — one is the remote, the
 * other is the government over it.
 *
 * Three routes:
 *
 * | what                            | route                              |
 * |---------------------------------|------------------------------------|
 * | which repository this project is| `GET /projects/{id}/github-repo`   |
 * | open pull requests              | `POST /github/requests` `pr_list`  |
 * | the last CI runs                | `POST /github/requests` `run_list` |
 *
 * **The mapping had to be built before any of this could be read**, and that is worth recording
 * because the shape of this module is the shape of that gap. Every typed read carries a repository
 * as `owner/name`, and in the núcleo `Repo::new` was reachable only from `ReadOp::from_request` — so
 * the repository was always something the CALLER named. `vcs::resolve_repo` answers a
 * confusingly-similar question with a folder on this disk, and `GET /projects/detect` reports a
 * remote URL but takes a path and belongs to the wizard that adds a project. Parsing that URL into a
 * slug here would have been this app inventing a mapping the daemon does not hold; instead the
 * daemon holds it, and {@link useProjectRepo} reads it.
 *
 * **Nothing here is polled and nothing here retries.** Every one of the three spawns a subprocess —
 * two of them a network call — and a timer would have this window running `gh` for ever in the
 * background to redraw a panel nobody is looking at. A failure is semantic: no token stays no token,
 * and a second identical request gets the same answer more slowly.
 */

/* ----------------------------------------------------------------- shapes -- */

/**
 * Which repository on GitHub a project is, or why there is no answer —
 * `github::ProjectRepo`, tagged on `state` by its `serde(tag)`.
 *
 * **Six arms and not a nullable string, because §5.1's whole demand is that the page EXPLAIN.**
 * *«um painel vazio é indistinguível de um repositório sem PRs»* — and a mapping that answered
 * `null` for every one of these would make that sentence unwritable: a project switched off, a
 * checkout somebody moved, a folder git does not know, a repository with no `origin` and a project
 * on GitLab would all arrive as the same nothing. Five of the six are ordinary states rather than
 * faults, and each wants different advice.
 */
export type ProjectRepo =
  /** `origin` is a GitHub repository. `repo` is what a read is sent; `remote` is what it came from. */
  | { state: "known"; repo: string; remote: string }
  /** On the roster with no folder named — what `off` mode leaves behind, its root cleared. */
  | { state: "no_root" }
  /** A folder was named and is not there. */
  | { state: "root_missing"; root: string }
  /** The folder is there and git keeps nothing in it. Only `active` insists on a repository. */
  | { state: "not_a_repository"; root: string }
  /** A repository with no `origin`. A local-only project is a project. */
  | { state: "no_remote"; root: string }
  /** There is an `origin` and it is not a GitHub repository. */
  | { state: "not_github"; remote: string };

/**
 * The two listing reads this page makes, by their typed names.
 *
 * A union rather than `string`, because these are the only two §5.1 asks for and the others are not
 * interchangeable with them: `pr_view` and `run_logs` are graded `ReadsUntrusted` in the núcleo —
 * they latch a turn — and a page that could ask for one by widening a type would be reaching past a
 * boundary drawn somewhere else.
 */
export type ListingRead = "pr_list" | "run_list";

/**
 * What one `gh` invocation said — the `"ran"` shape of `POST /github/requests`.
 *
 * **`stdout` is PROSE and there is no structured form of it.** `--json` is in the núcleo's
 * `REFUSED_READ_FLAGS`, deliberately and not as an oversight, and so are `--limit` and `-L`: the
 * grading that lets `pr_list` be `ReadsOwn` leans on `gh`'s own thirty-row cap, which
 * `github.rs` calls *"not a performance detail, it is half of this grading"*. So a caller renders
 * this text; it does not parse it, and asking for JSON to make a table would be spending a security
 * argument on a layout.
 *
 * **A non-zero `exit_code` arrives here and not as a refusal**, because `gh` ran: a repository that
 * has been renamed, a token without access to it, a network that is down. `output_tail` is stdout
 * then stderr, which is where that says why — the reason the page shows the tail rather than the
 * empty `stdout` when the code is not zero.
 */
export interface ReadOutcome {
  status: "ran";
  operation: string;
  /** `null` when the process was killed rather than exiting — a timeout tree-kill. */
  exit_code: number | null;
  /** Capped, redacted, and truncated at the START when the cap bites: the tail is where the error is. */
  stdout: string;
  /** stdout then stderr. What a person needs when `exit_code` is not zero. */
  output_tail: string;
}

/* ------------------------------------------------------------------ hooks -- */

/**
 * How long one answer serves every reader on the page that asked for it.
 *
 * Long enough that the readers one page mounts share a single subprocess, short enough that
 * leaving the page and coming back is still a fresh question. Not a poll: nothing re-asks when it
 * runs out; the next reader to mount does.
 */
const SHARED_BY_ONE_PAGE_MS = 30_000;

/**
 * Which repository on GitHub this project is — `GET /projects/{id}/github-repo`.
 *
 * Not polled: a project's `origin` changes when somebody types `git remote set-url`, which is not
 * something this window can watch for, and the read spawns git.
 *
 * A project the roster has never heard of is the one 404 here, and it stays an error — there is no
 * state to describe. Everything the filesystem and git can say is a 200 and an arm of
 * {@link ProjectRepo}.
 */
export function useProjectRepo(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.githubRepo(projectId ?? ""),
    queryFn: () =>
      apiFetch<ProjectRepo>(`/projects/${encodeURIComponent(projectId ?? "")}/github-repo`),
    enabled: projectId !== null && projectId !== "",
    retry: false,
    // A project's `origin` does not change while somebody reads a page about it, and this spawns
    // git — the same reading `useDetect` takes about the wizard's three git commands.
    refetchOnWindowFocus: false,
    // Several readers on one page: the Authority mode's status line, its remote and its GitHub
    // table each ask. With `staleTime` 0 a reader that mounted a beat after the answer landed
    // counted it stale and spawned git again. Half a minute covers one page's worth of readers;
    // coming back to the page later asks again, as it always did.
    staleTime: SHARED_BY_ONE_PAGE_MS,
  });
}

/**
 * One listing read, run by the daemon with the token from Credential Manager.
 *
 * **A `useQuery` over a POST, and the verb is the núcleo's shape rather than this call's meaning.**
 * `POST /github/requests` is the one door both GitHub tools come through — the partition between
 * reading and acting is held by the parameter TYPES at the tool boundary, never by the transport —
 * so a read arrives as a POST carrying a `ReadOp`. What this hook does is read, and a query is what
 * a page wants for it: cached, refetchable, and asked once per repository rather than once per
 * render.
 *
 * **Keyed by the repository and not by the project**, because that is what the answer is about. Two
 * projects rooted at two worktrees of one repository ask the same question and should share the
 * answer.
 *
 * `repo` of `null` is the whole of the "do not ask" case, and it is why the mapping is a separate
 * call: with no repository there is no request to make, and every one of §5.1's not-working cases is
 * either this being `null` or the request coming back refused.
 */
export function useGithubListing(op: ListingRead, repo: string | null) {
  return useQuery({
    queryKey: keys.github.listing(op, repo ?? ""),
    queryFn: () =>
      apiFetch<ReadOutcome>("/github/requests", {
        method: "POST",
        // Doubly named, and that is the wire shape: the route's body is `{op}` and the operation
        // itself is internally tagged on `op` as well.
        body: JSON.stringify({ op: { op, repo } }),
      }),
    enabled: repo !== null && repo !== "",
    retry: false,
    // **Load-bearing, not tidiness.** TanStack v5 refetches on window focus by default,
    // `createAppQueryClient` does not turn it off, and `staleTime` is 0 — so without this every
    // alt-tab back into a tray app spent two `gh` invocations and two GitHub API calls, which is the
    // polling this module's header says it does not do, wearing a different trigger. The one gesture
    // that asks again is the button.
    refetchOnWindowFocus: false,
    // **And not on a second reader's mount either.** The Authority mode reads these listings from
    // three places (its status line, the remote, the Reads group's "is GitHub answering?"), and they
    // do not always subscribe in one commit — measured: the later reader found the first answer
    // already stale and ran `gh` again, four invocations for two listings. The button still asks at
    // once (`refetch` ignores `staleTime`), and the page shows when the answer it has was read.
    staleTime: SHARED_BY_ONE_PAGE_MS,
  });
}
