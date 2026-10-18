import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { DAEMON_NAME, FORBIDDEN_DAEMON_WORD, FORBIDDEN_NO_ANSWER, TERMS, termKeys } from "./copy";

const ROOT = __dirname;

function tsxFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      if (name !== "node_modules") out.push(...tsxFiles(path));
    } else if (name.endsWith(".tsx") && !name.endsWith(".test.tsx")) {
      out.push(path);
    }
  }
  return out;
}

/** Drops block and line comments, which may name the daemon freely; what remains is rendered or logic. */
function withoutComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, "")
    // A CRLF checkout (core.autocrlf) leaves a `\r` that `.` will not match, so the line comment would survive.
    .split(/\r?\n/)
    .map((line) => line.replace(/(^|\s)\/\/.*$/, "$1"))
    .join("\n");
}

function offenders(): string[] {
  return tsxFiles(ROOT)
    .filter((path) => !path.includes(`${sep}test${sep}`))
    .filter((path) => {
      const code = withoutComments(readFileSync(path, "utf8"));
      return code.includes(FORBIDDEN_DAEMON_WORD) || code.includes(FORBIDDEN_NO_ANSWER);
    })
    .map((path) => relative(ROOT, path).split(sep).join("/"))
    .sort();
}

/**
 * Files that still print one of the forbidden words. This list only shrinks: W3 empties it
 * page by page, and a file that is not here may not start using either word.
 */
const STILL_TO_MIGRATE: readonly string[] = [
  "app/ConnectionGate.tsx",
  "app/KillSwitchControl.tsx",
  "app/NotificationsDrawer.tsx",
  "app/Sidebar.tsx",
  "app/UpdateNotice.tsx",
  "brain/BrainPanel.tsx",
  "brain/UnifiedList.tsx",
  "brain/knowledge/Chain.tsx",
  "brain/knowledge/KnowledgePanel.tsx",
  "brain/knowledge/ScopedMemory.tsx",
  "brain/knowledge/ScopedTools.tsx",
  "brain/knowledge/WaitingPanel.tsx",
  "canvas/Boundary.tsx",
  "canvas/FleetCanvas.tsx",
  "canvas/MapCanvas.tsx",
  "context/ContextRefs.tsx",
  "pages/Agents.tsx",
  "pages/Brain.tsx",
  "pages/Browser.tsx",
  "pages/CaptureWait.tsx",
  "pages/Chats.tsx",
  "pages/DistillerModel.tsx",
  "pages/EmbeddingModel.tsx",
  "pages/Feed.tsx",
  "pages/LoadoutPreview.tsx",
  "pages/MachineSettings.tsx",
  "pages/NotificationsView.tsx",
  "pages/RemoveProject.tsx",
  "pages/System.tsx",
  "pages/Voice.tsx",
  "pages/Waiting.tsx",
  "pages/Web.tsx",
  "project/AtlasLayers.tsx",
  "project/Branches.tsx",
  "project/Commands.tsx",
  "project/DecisionsWaiting.tsx",
  "project/DeleteFolder.tsx",
  "project/ExtractSpec.tsx",
  "project/GiveStamp.tsx",
  "project/IdeVerify.tsx",
  "project/ModeCode.tsx",
  "project/ModeGithub.tsx",
  "project/ModeMap.tsx",
  "project/ModeState.tsx",
  "project/Occupancy.tsx",
  "project/OnItsOwn.tsx",
  "project/Onboard.tsx",
  "project/OrphanCheck.tsx",
  "project/OwnedFiles.tsx",
  "project/SequenceView.tsx",
  "project/Settings.tsx",
  "project/StatesView.tsx",
  "project/TestsMap.tsx",
  "project/TriagePanel.tsx",
  "project/WorkflowGraph.tsx",
  "project/Workflows.tsx",
  "team/Work.tsx",
  "ui/CopyOnce.tsx",
  "ui/RefusalNote.tsx",
];

describe("copy glossary", () => {
  it("names the daemon in plain words", () => {
    expect(DAEMON_NAME).toBe("background service");
  });

  it("defines every term in one short sentence", () => {
    for (const key of termKeys()) {
      expect(TERMS[key].length).toBeGreaterThan(10);
      expect(TERMS[key].length).toBeLessThan(120);
    }
    expect(termKeys().sort()).toEqual(
      ["armed", "ceiling", "dossier", "gate", "seat", "shadow", "wheel"],
    );
  });

  it("lets no new file print a forbidden word", () => {
    const fresh = offenders().filter((file) => !STILL_TO_MIGRATE.includes(file));
    expect(fresh, `new files using "${FORBIDDEN_DAEMON_WORD}" or "${FORBIDDEN_NO_ANSWER}"`).toEqual([]);
  });

  it("keeps the exception list honest: every entry still offends", () => {
    const current = offenders();
    const healed = STILL_TO_MIGRATE.filter((file) => !current.includes(file));
    expect(healed, "remove these from STILL_TO_MIGRATE").toEqual([]);
  });
});
