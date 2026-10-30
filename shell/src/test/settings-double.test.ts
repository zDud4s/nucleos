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

async function refusalOf(answer: Promise<unknown>): Promise<ApiRefusal> {
  const error = await answer.catch((e: unknown) => e);
  expect(error).toBeInstanceOf(ApiRefusal);
  return error as ApiRefusal;
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
    expect((error as ApiRefusal).code).toBe("stale");
    expect((error as ApiRefusal).body).toEqual({ refusal: "stale", current: 2 });
  });

  it("refuses a stale rollback the way it refuses a stale write", async () => {
    const error = await refusalOf(
      seeded().handle("/settings/machine/voice/rollback/2", { method: "POST", body: JSON.stringify({ version: 1 }) }),
    );
    expect(error.status).toBe(409);
    expect(error.body).toEqual({ refusal: "stale", current: 2 });
  });

  it("refuses with 422 and a detail alone, which is all the handler sends", async () => {
    const double = settingsDouble([
      { scope: "machine", name: "voice", schema: SCHEMA, validate: () => ({ message: "unknown field `hot`" }) },
    ]);
    const error = await refusalOf(
      double.handle("/settings/machine/voice", { method: "PUT", body: JSON.stringify({ body: {}, version: 0 }) }),
    );
    expect(error.status).toBe(422);
    expect(error.code).toBe("invalid");
    expect(error.detail).toBe("unknown field `hot`");
    expect(error.body).toEqual({ refusal: "invalid", detail: "unknown field `hot`" });
  });

  it("adds the field to a 422 only when validate names one", async () => {
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
    expect(error.body).toEqual({ refusal: "invalid", field: "/hotkey", detail: "empty" });
  });

  it("checks the body before the version, as the store does", async () => {
    const double = settingsDouble([
      { scope: "machine", name: "voice", schema: SCHEMA, version: 5, validate: () => ({ message: "bad" }) },
    ]);
    const error = await refusalOf(
      double.handle("/settings/machine/voice", { method: "PUT", body: JSON.stringify({ body: {}, version: 7 }) }),
    );
    expect(error.status).toBe(422);
    expect(error.code).toBe("invalid");
  });

  it("plays back a queued failure once, named and worded the way the client parses it", async () => {
    const double = seeded();
    double.failNext = { status: 423, body: { refusal: "kill_switch" } };
    const put = (version: number) =>
      double.handle("/settings/machine/voice", { method: "PUT", body: JSON.stringify({ body: {}, version }) });
    const error = (await put(2).catch((e: unknown) => e)) as ApiRefusal;
    expect(error.status).toBe(423);
    expect(error.code).toBe("kill_switch");
    // With no `detail` beside it, the client makes the name the sentence.
    expect(error.detail).toBe("kill_switch");
    expect(await put(2)).toEqual({ version: 3 });
  });

  it("serves an unreadable row as a 422 with the version that replaces it, and says so in the index", async () => {
    const double = settingsDouble([
      { scope: "machine", name: "verify", schema: SCHEMA, version: 3, unreadable: "expected a mapping" },
    ]);
    const error = await refusalOf(double.handle("/settings/machine/verify", undefined));
    expect(error.status).toBe(422);
    expect(error.code).toBe("unreadable");
    expect(error.detail).toBe("expected a mapping");
    expect(error.body).toEqual({ refusal: "unreadable", detail: "expected a mapping", version: 3 });

    const rows = (await double.handle("/settings", undefined)) as { present: boolean; unreadable: boolean; version: number }[];
    expect(rows[0]).toMatchObject({ present: true, unreadable: true, version: 3 });
  });

  it("replaces an unreadable row at the version its refusal carried", async () => {
    const double = settingsDouble([
      { scope: "machine", name: "verify", schema: SCHEMA, version: 3, unreadable: "expected a mapping" },
    ]);
    const path = "/settings/machine/verify";
    expect(await double.handle(path, { method: "PUT", body: JSON.stringify({ body: { hotkey: "F1" }, version: 3 }) })).toEqual({
      version: 4,
    });
    expect(await double.handle(path, undefined)).toMatchObject({ body: { hotkey: "F1" }, version: 4 });
    const rows = (await double.handle("/settings", undefined)) as { unreadable: boolean }[];
    expect(rows[0].unreadable).toBe(false);
  });

  it("marks a readable row as readable in the index", async () => {
    const rows = (await seeded().handle("/settings", undefined)) as { unreadable: boolean }[];
    expect(rows.map((row) => row.unreadable)).toEqual([false, false]);
  });

  it("keeps a channel's name as one encoded segment", async () => {
    const double = settingsDouble([
      { scope: "machine", name: "channels/telegram", schema: SCHEMA, body: { hotkey: "F9" }, version: 1 },
    ]);
    const path = "/settings/machine/channels%2Ftelegram";
    expect(await double.handle(path, undefined)).toMatchObject({ version: 1 });
    await double.handle(path, { method: "PUT", body: JSON.stringify({ body: {}, version: 1 }) });
    expect(double.writes[0].name).toBe("channels/telegram");
    const history = (await double.handle(`${path}/history`, undefined)) as { version: number }[];
    expect(history.map((row) => row.version)).toEqual([2, 1]);
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

  it("refuses a rollback to a version that never existed", async () => {
    const error = await refusalOf(
      seeded().handle("/settings/machine/voice/rollback/99", { method: "POST", body: JSON.stringify({ version: 2 }) }),
    );
    expect(error.status).toBe(404);
    expect(error.body).toEqual({ refusal: "no_such_version" });
  });

  it("answers 404 unknown_document when reading a document nobody seeded", async () => {
    const error = await refusalOf(seeded().handle("/settings/machine/nope", undefined));
    expect(error.status).toBe(404);
    expect(error.code).toBe("unknown_document");
    expect(error.body).toEqual({ refusal: "unknown_document" });
  });

  it("answers 403 not_ours when writing a document nobody seeded", async () => {
    const error = await refusalOf(
      seeded().handle("/settings/machine/nope", { method: "PUT", body: JSON.stringify({ body: {}, version: 0 }) }),
    );
    expect(error.status).toBe(403);
    expect(error.body).toEqual({ refusal: "not_ours" });
  });
});
