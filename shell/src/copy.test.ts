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
    .split("\n")
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
  "analytics/AnalyticsView.tsx",
  "analytics/DevtimeSection.tsx",
  "analytics/PipelineSection.tsx",
  "analytics/sections.tsx",
  "app/ConnectionGate.tsx",
  "app/Dictation.tsx",
  "app/FeedEmbed.tsx",
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
  "calendar/DaySheet.tsx",
  "canvas/Boundary.tsx",
  "canvas/FleetCanvas.tsx",
  "canvas/MapCanvas.tsx",
  "chats/ProjectPicker.tsx",
  "context/ContextRefs.tsx",
  "pages/Agents.tsx",
  "pages/Autopilot.tsx",
  "pages/AutopilotJudge.tsx",
  "pages/AutopilotRecords.tsx",
  "pages/AutopilotResolve.tsx",
  "pages/Brain.tsx",
  "pages/Browser.tsx",
  "pages/Calendar.tsx",
  "pages/CaptureWait.tsx",
  "pages/Chats.tsx",
  "pages/Contacts.tsx",
  "pages/Council.tsx",
  "pages/DistillerModel.tsx",
  "pages/EmbeddingModel.tsx",
  "pages/Feed.tsx",
  "pages/Files.tsx",
  "pages/Fleet.tsx",
  "pages/Home.tsx",
  "pages/LoadoutPreview.tsx",
  "pages/MachineSettings.tsx",
  "pages/Mail.tsx",
  "pages/MailDetail.tsx",
  "pages/NewProject.tsx",
  "pages/NotificationsView.tsx",
  "pages/Projects.tsx",
  "pages/RemoveProject.tsx",
  "pages/Roster.tsx",
  "pages/RunDetail.tsx",
  "pages/Runs.tsx",
  "pages/System.tsx",
  "pages/TeamRunDetail.tsx",
  "pages/Teams.tsx",
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
  "team/Bench.tsx",
  "team/Decisions.tsx",
  "team/Work.tsx",
  "ui/CopyOnce.tsx",
  "ui/RefusalNote.tsx",
  "ui/RunPipeline.tsx",
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
