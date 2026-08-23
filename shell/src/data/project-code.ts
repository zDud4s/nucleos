import { useQuery } from "@tanstack/react-query";
import { apiFetch, apiText } from "./client";
import { keys } from "./keys";

/**
 * Reading a project as one run left it.
 *
 * The subject of every read here is the **run**, not the repository, and that is what separates a
 * review surface from a file browser that happens to be read-only. A browser opens on four thousand
 * files; this opens on the dozen a run touched and says how many nobody did.
 *
 * All of it is on demand and never polled: these routes walk somebody's repository with a
 * subprocess, and a three-second timer would mean spawning git forever in the background for an
 * answer that changes when an agent writes a file.
 */

/** What a run changed, and how big the tree it changed it in is. */
export interface Changed {
  paths: string[];
  /**
   * Files tracked in that worktree.
   *
   * The subtraction is the whole point of showing it: "twelve changed, and 3,214 nobody touched" is
   * the sentence that makes the panel a review surface. A bare list of twelve says nothing about
   * how much was left alone.
   */
  tracked: number;
}

/** One line of a file, and who last touched it. */
export interface BlameLine {
  line: number;
  sha: string;
  author: string;
  /** Epoch seconds, as git reports them. */
  at: number;
  summary: string;
  text: string;
  /**
   * This line is in the working tree and in no commit.
   *
   * Git fills the rest of the header with placeholders for these — "Not Committed Yet" as the
   * author, right now as the date — so a panel that drew the row like any other would credit a
   * person who never wrote the line.
   */
  uncommitted: boolean;
}

/**
 * What a run changed since it branched.
 *
 * Refused with 422 when the daemon never recorded the branch point, and with 404 when the worktree
 * is gone or belongs to another project. Those are three different answers and none of them is an
 * empty change set.
 */
export function useChanged(projectId: string | null, run: number | null) {
  return useQuery({
    queryKey: keys.projects.changed(projectId ?? "", run ?? 0),
    queryFn: () =>
      apiFetch<Changed>(
        `/projects/${encodeURIComponent(projectId ?? "")}/changed?run=${run ?? 0}`,
      ),
    enabled: projectId !== null && projectId !== "" && run !== null,
    retry: false,
  });
}

/**
 * The diff of one file, or of everything, as this run changed it.
 *
 * `apiText`, never `apiFetch`: the body is a diff and not JSON, and an empty diff is a 200 with an
 * empty body — which is a real answer here, not a parse failure.
 */
export function useRunDiff(projectId: string | null, run: number | null, path: string) {
  return useQuery({
    queryKey: keys.projects.runDiff(projectId ?? "", run ?? 0, path),
    queryFn: () =>
      apiText(
        `/projects/${encodeURIComponent(projectId ?? "")}/diff?run=${run ?? 0}&path=${encodeURIComponent(path)}`,
      ),
    enabled: projectId !== null && projectId !== "" && run !== null,
    retry: false,
  });
}

/** One file, as this run has it. */
export function useRunFile(projectId: string | null, run: number | null, path: string) {
  return useQuery({
    queryKey: keys.projects.runFile(projectId ?? "", run ?? 0, path),
    queryFn: () =>
      apiText(
        `/projects/${encodeURIComponent(projectId ?? "")}/cat?run=${run ?? 0}&path=${encodeURIComponent(path)}`,
      ),
    enabled: projectId !== null && projectId !== "" && run !== null && path !== "",
    retry: false,
  });
}

/** Who last touched each line of a file, as this run has it. */
export function useRunBlame(projectId: string | null, run: number | null, path: string) {
  return useQuery({
    queryKey: keys.projects.runBlame(projectId ?? "", run ?? 0, path),
    queryFn: () =>
      apiFetch<BlameLine[]>(
        `/projects/${encodeURIComponent(projectId ?? "")}/blame?run=${run ?? 0}&path=${encodeURIComponent(path)}`,
      ),
    enabled: projectId !== null && projectId !== "" && run !== null && path !== "",
    retry: false,
  });
}

/** Where one run's checkout is, and what it was cut from. */
export interface Worktree {
  /** Absolute, on this machine. Every door to the editor is built by joining onto this. */
  path: string;
  branch: string;
  /** `null` when the daemon never recorded a branch point — which is why nothing can be measured. */
  base_sha: string | null;
  created_at: string;
}

export function useRunWorktree(projectId: string | null, run: number | null) {
  return useQuery({
    queryKey: keys.projects.worktree(projectId ?? "", run ?? 0),
    queryFn: () =>
      apiFetch<Worktree>(
        `/projects/${encodeURIComponent(projectId ?? "")}/worktree?run=${run ?? 0}`,
      ),
    enabled: projectId !== null && projectId !== "" && run !== null,
    retry: false,
  });
}

/**
 * A repository-relative path, made absolute against a worktree.
 *
 * The editor's URL handler takes a full path and nothing else, and the shell holds neither half on
 * its own: the daemon reports paths relative to the repository, and a run's worktree is not under
 * the project root but beside it. A link built from the relative half alone opens nothing and says
 * nothing about why — which is the worst way for a seam to fail, so this is a function with a test
 * rather than a template literal at four call sites.
 *
 * The separator is a forward slash whichever way the worktree spells its own, because `vscodeUrl`
 * normalises them anyway and mixing them here would only make the result harder to read in a test.
 */
export function absolutePath(worktree: string | undefined, rel: string): string | null {
  if (worktree === undefined || worktree === "") return null;
  if (rel === "") return worktree;
  return `${worktree.replace(/[\\/]+$/, "")}/${rel}`;
}

/**
 * A changed-file list as a tree, one level at a time.
 *
 * Directories with a single child are collapsed into their parent — `core/src/http.rs` reads as one
 * row and not as three — because a review of twelve files should not be twenty rows of scaffolding.
 * That is the one thing this does beyond grouping, and it is why it is a function with a test
 * rather than a `reduce` inside the component.
 */
export interface TreeNode {
  /** What is shown: the collapsed path segment, or the file name. */
  label: string;
  /** The full path, for a file. `null` for a directory. */
  path: string | null;
  children: TreeNode[];
}

export function asTree(paths: string[]): TreeNode[] {
  interface Building {
    children: Map<string, Building>;
    path: string | null;
  }

  const root: Building = { children: new Map(), path: null };
  for (const path of [...paths].sort()) {
    let node = root;
    const parts = path.split("/").filter((part) => part !== "");
    parts.forEach((part, index) => {
      let next = node.children.get(part);
      if (next === undefined) {
        next = { children: new Map(), path: null };
        node.children.set(part, next);
      }
      if (index === parts.length - 1) next.path = path;
      node = next;
    });
  }

  function collapse(label: string, node: Building): TreeNode {
    // A directory with exactly one child and no file of its own is scaffolding, so it is folded
    // into the name of what it contains.
    if (node.path === null && node.children.size === 1) {
      const [childLabel, child] = [...node.children.entries()][0];
      const folded = collapse(childLabel, child);
      return { ...folded, label: `${label}/${folded.label}` };
    }
    return {
      label,
      path: node.path,
      children: [...node.children.entries()]
        .map(([childLabel, child]) => collapse(childLabel, child))
        // Directories first, then files, each alphabetical — the same order `ls` answers in.
        .sort((a, b) => {
          const aDir = a.path === null ? 0 : 1;
          const bDir = b.path === null ? 0 : 1;
          return aDir - bDir || a.label.localeCompare(b.label);
        }),
    };
  }

  return [...root.children.entries()].map(([label, node]) => collapse(label, node));
}
