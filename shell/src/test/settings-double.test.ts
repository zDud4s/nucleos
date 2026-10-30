import { describe, expect, it } from "vitest";
import { ApiRefusal } from "../data/client";
import { isSettingsPath, settingsDouble } from "./settings-double";

const SCHEMA = { type: "object", properties: { hotkey: { type: "string" } } };

function seeded() {
  return settingsDouble([
    { scope: "machine", name: "voice", schema: SCHEMA, body: { hotkey: "F9" }, version: 2, takes_effect: "when the daemon restarts" },
    { scope: "project:alpha", name: "autopilot", schema: SCHEMA },
  ]);
}

describe("isSettingsPath", () => {
  it("matches the settings routes and nothing else", () => {
    expect(isSettingsPath("/settings")).toBe(true);
    expect(isSettingsPath("/settings?scope=machine")).toBe(true);
    expect(isSettingsPath("/settings/machine/voice")).toBe(true);
    expect(isSettingsPath("/settings-other")).toBe(false);
    expect(isSettingsPath("/config/secrets")).toBe(false);
  });
});

describe("the double", () => {
  it("lists the index, filtered by ?scope", async () => {
    const double = seeded();
    const all = (await double.handle("/settings", undefined)) as { scope: string; present: boolean }[];
    expect(all.map((row) => row.scope).sort()).toEqual(["machine", "project:alpha"]);
    const project = (await double.handle("/settings?scope=project%3Aalpha", undefined)) as { name: string }[];
    expect(project.map((row) => row.name)).toEqual(["autopilot"]);
  });

  it("serves a never-written document at version 0", async () => {
    const doc = (await seeded().handle("/settings/project%3Aalpha/autopilot", undefined)) as {
      version: number;
    };
    expect(doc.version).toBe(0);
  });

  it("writes, bumps the version and keeps the history", async () => {
    const double = seeded();
    const path = "/settings/machine/voice";
    const result = await double.handle(path, { method: "PUT", body: JSON.stringify({ body: { hotkey: "F10" }, version: 2 }) });
    expect(result).toEqual({ version: 3 });
    expect(double.writes).toHaveLength(1);
    const history = (await double.handle(`${path}/history`, undefined)) as { version: number }[];
    expect(history.map((row) => row.version)).toEqual([3, 2]);
  });

  it("refuses a stale write with a 409 that carries the current version", async () => {
    const double = seeded();
    const error = await double
      .handle("/settings/machine/voice", { method: "PUT", body: JSON.stringify({ body: {}, version: 1 }) })
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ApiRefusal);
    expect((error as ApiRefusal).status).toBe(409);
    expect((error as ApiRefusal).body).toEqual({ error: "stale", current: 2 });
  });

  it("refuses with 422 and the field when validate says so", async () => {
    const double = settingsDouble([
      {
        scope: "machine",
        name: "voice",
        schema: SCHEMA,
        validate: (body) => ((body as { hotkey?: string }).hotkey === "" ? { field: "/hotkey", message: "empty" } : null),
      },
    ]);
    const error = (await double
      .handle("/settings/machine/voice", { method: "PUT", body: JSON.stringify({ body: { hotkey: "" }, version: 0 }) })
      .catch((e: unknown) => e)) as ApiRefusal;
    expect(error.status).toBe(422);
    expect(error.body).toEqual({ error: "invalid", field: "/hotkey", message: "empty" });
  });

  it("plays back a queued failure once", async () => {
    const double = seeded();
    double.failNext = { status: 423, body: { error: "kill_switch" } };
    const put = (version: number) =>
      double.handle("/settings/machine/voice", { method: "PUT", body: JSON.stringify({ body: {}, version }) });
    const error = (await put(2).catch((e: unknown) => e)) as ApiRefusal;
    expect(error.status).toBe(423);
    expect(await put(2)).toEqual({ version: 3 });
  });

  it("rolls back as a new version", async () => {
    const double = seeded();
    const path = "/settings/machine/voice";
    await double.handle(path, { method: "PUT", body: JSON.stringify({ body: { hotkey: "F10" }, version: 2 }) });
    const result = await double.handle(`${path}/rollback/2`, { method: "POST", body: JSON.stringify({ version: 3 }) });
    expect(result).toEqual({ version: 4 });
    const doc = (await double.handle(path, undefined)) as { body: unknown };
    expect(doc.body).toEqual({ hotkey: "F9" });
  });

  it("answers 404 for a document nobody seeded", async () => {
    const error = (await seeded().handle("/settings/machine/nope", undefined).catch((e: unknown) => e)) as ApiRefusal;
    expect(error.status).toBe(404);
  });
});
