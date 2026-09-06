/**
 * Manual measurement of the CLI's own advertised tool surface against `runner.rs`'s blocklist.
 *
 * # Why this exists
 *
 * `core/src/runner.rs`'s `BUILTIN_TOOLS` is a BLOCKLIST: `denied_tools()` denies exactly these
 * names under `ToolPolicy::McpOnly`, so any tool the CLI advertises that is absent from the list
 * is ALLOWED to an assistant turn — the surface that reads summaries of mail written by strangers.
 * The failure mode measured 2026-09 was `TaskCreate`/`TaskGet`/`TaskList`/`TaskUpdate` appearing
 * inside a version the list had already been measured against, and the only thing that noticed was
 * a dead run in production: `advertised_tools_violate` kills a turn at its `init` event rather than
 * a gate turning red before anyone shipped. This script is the same measurement, run BY HAND right
 * after a `claude update`, before that upgrade ever reaches a real conversation.
 *
 * # Why it reads the source and not the daemon's `/api/...` route
 *
 * `core/src/http.rs`'s `get_deniable_tools` serves the same list to the UI, but reaching it needs a
 * running, authenticated daemon. The whole point of this script is to be run standalone right after
 * a CLI upgrade, so it parses `BUILTIN_TOOLS` out of `core/src/runner.rs` directly — the source is
 * equally the one list, and a script that needs nothing but a checkout is a script that actually
 * gets run.
 *
 * # What a diff means
 *
 * A tool the CLI now advertises that the blocklist does not deny is the regression this exists to
 * catch: exit 1, and the missing names are printed so `BUILTIN_TOOLS` can be extended before the
 * next release reaches a conversation.
 *
 * A blocklist name ABSENT from what the CLI currently advertises is NOT an error and is expected:
 * the list is deliberately wider than any one version's tool set (see the doc comment on
 * `BUILTIN_TOOLS` itself) — denying a name the CLI does not have costs one harmless stderr line, and
 * shrinking the list back to exactly one version's surface is how the next upgrade's dead run gets
 * written again.
 *
 * # Usage
 *
 *   node scripts/tool-surface.mjs             measure the installed `claude` CLI
 *   node scripts/tool-surface.mjs --self-test  no CLI required; proves the diff logic itself by
 *                                               dropping one real blocklist name and asserting the
 *                                               script reports it missing. Exits 1 on success (the
 *                                               exit code a genuine gap would produce), 2 if the
 *                                               diff logic itself is broken.
 */
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const RUNNER_RS = join(REPO, "core", "src", "runner.rs");

/** How long the trivial probe turn gets before it counts as hung rather than merely slow. */
const PROBE_TIMEOUT_MS = 30_000;

/**
 * Parses the `BUILTIN_TOOLS` string-literal array out of `runner.rs`, rather than depending on
 * `serde_json` or a Rust toolchain being available to a script that must run on a bare checkout.
 * The const's own shape (`&[&str] = &[ ... ];`, one quoted name per line) is stable enough that a
 * plain regex over the slice between the two markers is the whole parser.
 */
function parseBuiltinTools(source) {
  const start = source.indexOf("pub(crate) const BUILTIN_TOOLS: &[&str] = &[");
  if (start === -1) {
    throw new Error("BUILTIN_TOOLS const not found in runner.rs — has it been renamed?");
  }
  const end = source.indexOf("];", start);
  if (end === -1) {
    throw new Error("BUILTIN_TOOLS const's closing `];` not found in runner.rs");
  }
  const body = source.slice(start, end);
  const names = [...body.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  if (names.length === 0) {
    throw new Error("BUILTIN_TOOLS parsed to zero names — the regex or the const shape moved");
  }
  return names;
}

/** Runs a trivial, side-effect-free probe turn and returns the `tools` array off its `init` event. */
async function advertisedTools() {
  return new Promise((resolvePromise, reject) => {
    // On Windows the installed binary is a `.cmd` shim (`npm`'s standard wrapper), and
    // `child_process.spawn` does not consult `PATHEXT` the way a real shell does — it must be
    // asked to run through one, or the bare name comes back `ENOENT` despite being on PATH.
    //
    // `--strict-mcp-config` (no `--mcp-config` given) means the probe advertises NO MCP server at
    // all, ambient or otherwise — this measures the CLI's own BUILT-IN tool set, which is the whole
    // of what `BUILTIN_TOOLS` governs. Without it, whatever MCP plugins happen to be configured on
    // the machine running this script (a `claude-mem` search plugin, a project's own servers, ...)
    // would show up as "advertised but not in BUILTIN_TOOLS" and be reported as a false gap — those
    // names were never candidates for the blocklist, which denies BUILT-INS, never MCP tools.
    const probeArgs = ["-p", "say hi", "--output-format", "stream-json", "--verbose", "--strict-mcp-config"];
    const onWindows = process.platform === "win32";
    // Every argument above is a static literal, never user input, so quoting them into one shell
    // string for Windows carries no injection risk — it only avoids Node's shell-argv escaping
    // warning, which assumes an attacker-controlled argument that cannot occur here.
    const child = onWindows
      ? spawn(`claude ${probeArgs.map((a) => `"${a}"`).join(" ")}`, {
          stdio: ["ignore", "pipe", "pipe"],
          shell: true,
        })
      : spawn("claude", probeArgs, { stdio: ["ignore", "pipe", "pipe"] });

    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error(`claude -p did not answer within ${PROBE_TIMEOUT_MS}ms`));
    }, PROBE_TIMEOUT_MS);

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
    child.on("close", () => {
      clearTimeout(timer);
      for (const line of stdout.split("\n")) {
        const trimmed = line.trim();
        if (!trimmed) continue;
        let value;
        try {
          value = JSON.parse(trimmed);
        } catch {
          continue;
        }
        if (value?.type === "system" && value?.subtype === "init" && Array.isArray(value?.tools)) {
          resolvePromise(value.tools);
          return;
        }
      }
      reject(
        new Error(
          `no system/init event with a "tools" array in the stream.\nstderr:\n${stderr}`,
        ),
      );
    });
  });
}

/** Pure: the whole comparison, so `--self-test` can exercise it with no process spawned. */
function diffAgainstBlocklist(advertised, blocklist) {
  const denied = new Set(blocklist);
  return advertised.filter((tool) => !denied.has(tool));
}

async function main() {
  const selfTest = process.argv.includes("--self-test");
  const source = readFileSync(RUNNER_RS, "utf8");
  const blocklist = parseBuiltinTools(source);

  if (selfTest) {
    // No CLI required: drop one real name from the blocklist and assert the tool that name
    // represents is reported missing, proving the diff direction is the one this script claims.
    const dropped = blocklist[0];
    const shrunk = blocklist.slice(1);
    const missing = diffAgainstBlocklist([dropped, ...blocklist.slice(1, 4)], shrunk);
    if (missing.length !== 1 || missing[0] !== dropped) {
      console.error(
        `--self-test FAILED: expected exactly ["${dropped}"] missing, got ${JSON.stringify(missing)}`,
      );
      process.exit(2); // A bug in the diff logic itself — distinct from the exit code it measures.
    }
    console.log(
      `--self-test passed: a dropped name ("${dropped}") is correctly reported missing.\n` +
        `Exiting 1 to demonstrate the real script's exit code on a genuine gap.`,
    );
    process.exit(1); // The exit code a real CLI-advertised gap would produce.
  }

  let advertised;
  try {
    advertised = await advertisedTools();
  } catch (err) {
    console.error(`could not measure the CLI: ${err.message}`);
    process.exit(1);
    return;
  }

  const missing = diffAgainstBlocklist(advertised, blocklist);
  if (missing.length > 0) {
    console.error(
      `${missing.length} tool(s) advertised by the CLI are NOT in BUILTIN_TOOLS and are therefore ` +
        `ALLOWED under ToolPolicy::McpOnly:\n  ${missing.join("\n  ")}\n` +
        `Add them to core/src/runner.rs's BUILTIN_TOOLS before this reaches a real conversation.`,
    );
    process.exit(1);
  }

  console.log(
    `OK: all ${advertised.length} tools the CLI advertised are denied by BUILTIN_TOOLS ` +
      `(${blocklist.length} names on the list; the surplus is deliberate margin).`,
  );
  process.exit(0);
}

main();
