import { describe, expect, it } from "vitest";
import { diskPathFor, middleEllipsis, runsOnOpen, trashDaysLeft } from "./files";

describe("middleEllipsis", () => {
  it("leaves a name that fits alone", () => {
    expect(middleEllipsis("report.docx", 20)).toBe("report.docx");
  });

  it("cuts in the middle and keeps the extension, so two versions stay two", () => {
    const v2 = middleEllipsis("quarterly-board-pack-final-reviewed-v2.pdf", 24);
    const v3 = middleEllipsis("quarterly-board-pack-final-reviewed-v3.pdf", 24);
    expect(v2).toHaveLength(24);
    expect(v2.endsWith("-v2.pdf")).toBe(true);
    expect(v2).not.toBe(v3);
    expect(v2).toContain("…");
  });
});

describe("trashDaysLeft", () => {
  const day = 86_400_000;
  const deleted = "2026-09-01T00:00:00Z";
  const at = new Date(deleted).getTime();

  it("counts down from the retention", () => {
    expect(trashDaysLeft(deleted, at)).toBe(30);
    expect(trashDaysLeft(deleted, at + 3 * day)).toBe(27);
  });

  it("never goes below zero while the purge has yet to run", () => {
    expect(trashDaysLeft(deleted, at + 40 * day)).toBe(0);
  });
});

describe("diskPathFor", () => {
  it("lands where the daemon's ProjectDirs puts the root on each platform", () => {
    expect(diskPathFor("C:\\Users\\ana\\AppData\\Local", "invoices/march.pdf")).toBe(
      "C:\\Users\\ana\\AppData\\Local\\nucleos\\NucleOS\\data\\files\\invoices\\march.pdf",
    );
    expect(diskPathFor("/Users/ana/Library/Application Support", "a.txt")).toBe(
      "/Users/ana/Library/Application Support/dev.nucleos.NucleOS/files/a.txt",
    );
    expect(diskPathFor("/home/ana/.local/share/", "a.txt")).toBe("/home/ana/.local/share/nucleos/files/a.txt");
  });

  it("names the root itself for the empty path", () => {
    expect(diskPathFor("C:\\Users\\ana\\AppData\\Local", "")).toBe(
      "C:\\Users\\ana\\AppData\\Local\\nucleos\\NucleOS\\data\\files",
    );
  });
});

describe("runsOnOpen", () => {
  it("catches what Windows would run, in any case", () => {
    expect(runsOnOpen("setup.EXE")).toBe(true);
    expect(runsOnOpen("tools/fix.ps1")).toBe(true);
    expect(runsOnOpen("shortcut.lnk")).toBe(true);
  });

  it("leaves documents, and names with no extension, alone", () => {
    expect(runsOnOpen("report.pdf")).toBe(false);
    expect(runsOnOpen("Makefile")).toBe(false);
    expect(runsOnOpen(".bat")).toBe(false);
  });
});
