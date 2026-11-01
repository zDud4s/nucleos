import { describe, expect, it } from "vitest";
import type { ModelChoice } from "../data/chats";
import { routeOf, splitByRoute } from "./model-routes";

function choice(id: string, runner?: string | null, brain: ModelChoice["brain"] = "cloud"): ModelChoice {
  return { id, label: id, brain, efforts: [], runner };
}

describe("routeOf", () => {
  it("names agy and gemini-api rows by their runner field, never by id", () => {
    // The id says "claude" and "gemini"; only the runner field decides.
    const agy = routeOf(choice("agy/claude-sonnet-4-6", "agy"));
    expect(agy).toMatchObject({ runner: "agy", section: "Antigravity CLI", badge: "agy CLI", mark: "agy" });
    const api = routeOf(choice("gemini-2.5-pro", "gemini-api", "openrouter"));
    expect(api).toMatchObject({
      runner: "gemini-api",
      section: "Google Gemini API",
      badge: "Gemini API",
      mark: "gemini",
    });
    // An id that looks like agy's, without the runner field, is not a route.
    expect(routeOf(choice("agy/claude-sonnet-4-6"))).toBeNull();
  });

  it("is null for claude, codex and absent runners", () => {
    expect(routeOf(choice("sonnet", "claude"))).toBeNull();
    expect(routeOf(choice("gpt-5.5", "codex"))).toBeNull();
    expect(routeOf(choice("sonnet", null))).toBeNull();
    expect(routeOf(choice("sonnet", undefined))).toBeNull();
  });

  it("gives an unknown runner its own section titled with the runner id", () => {
    expect(routeOf(choice("x/model", "newcli"))).toEqual({
      runner: "newcli",
      section: "newcli",
      badge: "newcli",
      mark: "newcli",
    });
  });
});

describe("splitByRoute", () => {
  it("keeps house rows out of route sections and the reverse", () => {
    const sonnet = choice("sonnet", "claude");
    const plain = choice("opus");
    const gpt = choice("gpt-5.5", "codex");
    const agyRow = choice("agy/claude-sonnet-4-6", "agy");
    const api = choice("gemini-2.5-pro", "gemini-api", "openrouter");
    const other = choice("x/model", "newcli");

    // A duplicate id (a model listed in both the flat list and a group) appears once.
    const split = splitByRoute([other, agyRow, sonnet, api, plain, agyRow, gpt]);

    expect(split.house.map((c) => c.id)).toEqual(["sonnet", "opus", "gpt-5.5"]);
    // Known routes first in KNOWN order (agy, then gemini-api), then the rest by name.
    expect(split.routes.map((r) => r.route.runner)).toEqual(["agy", "gemini-api", "newcli"]);
    expect(split.routes[0].models.map((c) => c.id)).toEqual(["agy/claude-sonnet-4-6"]);
    expect(split.routes[1].models.map((c) => c.id)).toEqual(["gemini-2.5-pro"]);
    expect(split.routes[2].models.map((c) => c.id)).toEqual(["x/model"]);
    const inRoutes = split.routes.flatMap((r) => r.models.map((c) => c.id));
    expect(inRoutes).not.toContain("sonnet");
    expect(split.house.map((c) => c.id)).not.toContain("agy/claude-sonnet-4-6");
  });
});
