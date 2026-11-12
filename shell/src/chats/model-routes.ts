import type { ModelChoice } from "../data/chats";

/** How the model menu names a route that is not one of the house CLIs. */
export interface RouteInfo {
  runner: string;
  /** The submenu heading. */
  section: string;
  /** The short tag drawn beside each row. */
  badge: string;
  /** The `ProviderMark` key; an unknown one draws its initial. */
  mark: string;
}

/** Rows drawn by company, with no badge. */
const HOUSE = new Set(["claude", "codex"]);

/** The routes the shell knows by name, in the order their sections are drawn. */
const KNOWN: Record<string, Omit<RouteInfo, "runner">> = {
  agy: { section: "Antigravity CLI", badge: "agy CLI", mark: "agy" },
  "gemini-api": { section: "Google Gemini API", badge: "Gemini API", mark: "gemini" },
};

/**
 * The route a row travels, read from the daemon's `runner` field and never from the id.
 * Null for the house CLIs and for a row that names no runner.
 */
export function routeOf(c: ModelChoice): RouteInfo | null {
  const runner = c.runner;
  if (runner === null || runner === undefined || runner === "" || HOUSE.has(runner)) return null;
  const known = Object.prototype.hasOwnProperty.call(KNOWN, runner) ? KNOWN[runner] : undefined;
  return { runner, ...(known ?? { section: runner, badge: runner, mark: runner }) };
}

/**
 * Splits a menu into the house rows and one section per route. Each id appears once (a model can
 * be listed in both the flat list and a group). Routes come in KNOWN order, then the rest by name.
 */
export function splitByRoute(choices: ModelChoice[]): {
  house: ModelChoice[];
  routes: { route: RouteInfo; models: ModelChoice[] }[];
} {
  const seen = new Set<string>();
  const house: ModelChoice[] = [];
  const byRunner = new Map<string, { route: RouteInfo; models: ModelChoice[] }>();
  for (const choice of choices) {
    if (seen.has(choice.id)) continue;
    seen.add(choice.id);
    const route = routeOf(choice);
    if (route === null) {
      house.push(choice);
      continue;
    }
    const section = byRunner.get(route.runner);
    if (section) section.models.push(choice);
    else byRunner.set(route.runner, { route, models: [choice] });
  }
  const known = Object.keys(KNOWN);
  const rank = (runner: string) => {
    const at = known.indexOf(runner);
    return at === -1 ? known.length : at;
  };
  const routes = [...byRunner.values()].sort(
    (a, b) =>
      rank(a.route.runner) - rank(b.route.runner) || a.route.runner.localeCompare(b.route.runner),
  );
  return { house, routes };
}
