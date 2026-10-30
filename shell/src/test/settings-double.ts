import { ApiRefusal } from "../data/client";
import type { SettingIndexRow, SettingVersion } from "../data/settings";
import type { SchemaNode } from "../settings/schema";

/**
 * An in-memory `/settings` server for tests. It answers the routes the shell uses, keeps a version per
 * document and a snapshot per version, and refuses the way the núcleo does — with an `ApiRefusal` that
 * carries the status and the parsed body — so a page under test sees exactly the failures it would see
 * for real.
 *
 * Tests steer it with `failNext` (the next write or rollback fails with a canned refusal),
 * `validate` (a per-document check that answers 422), `unreadable` (the stored row does not parse) and
 * `external` (somebody else wrote the document, so the next save is stale).
 */
export interface SeedDoc {
  scope: string;
  name: string;
  schema: SchemaNode;
  body?: unknown;
  version?: number;
  takes_effect?: string;
  history?: SettingVersion[];
  /**
   * Test-only shape: `message` goes out as the wire's `detail`, and `field` is included only when named.
   * The handler sends no `field` today; a test that names one pins what the form would do with it.
   */
  validate?: (body: unknown) => { field?: string; message: string } | null;
  /** Why the stored row does not parse. The GET answers 422 until a write replaces it. */
  unreadable?: string;
}

/** The wire shape of a refusal: a name, an optional sentence, and whatever else the route adds. */
interface RefusalBody {
  refusal: string;
  detail?: string;
  [key: string]: unknown;
}

interface Stored {
  seed: SeedDoc;
  body: unknown;
  version: number;
  unreadable: string | null;
  history: SettingVersion[];
  snapshots: Map<number, unknown>;
}

export interface SettingsDouble {
  /** Every accepted write, in order. */
  writes: { scope: string; name: string; body: unknown; version: number }[];
  /** Every accepted rollback, in order. */
  rollbacks: { scope: string; name: string; target: number; current: number }[];
  /** The next write or rollback fails with this, once. */
  failNext: { status: number; body: RefusalBody } | null;
  /** Writes the document as somebody else would: the version moves and the next save is stale. */
  external: (scope: string, name: string, body: unknown) => void;
  handle: (path: string, init: RequestInit | undefined) => Promise<unknown>;
}

export function isSettingsPath(path: string): boolean {
  return path === "/settings" || path.startsWith("/settings/") || path.startsWith("/settings?");
}

const NOW = "2026-10-11T10:00:00Z";

/** Built as `client.ts` builds it: the name is the code, and `detail` wins as the sentence. */
function refuse(status: number, body: RefusalBody): ApiRefusal {
  return new ApiRefusal(status, body.refusal, body.detail ?? body.refusal, body);
}

export function settingsDouble(seed: SeedDoc[]): SettingsDouble {
  const docs = new Map<string, Stored>();
  const keyOf = (scope: string, name: string) => `${scope}\u0000${name}`;
  for (const doc of seed) {
    const version = doc.version ?? 0;
    const snapshots = new Map<number, unknown>();
    if (version > 0) snapshots.set(version, doc.body ?? {});
    docs.set(keyOf(doc.scope, doc.name), {
      seed: doc,
      body: doc.body ?? {},
      version,
      unreadable: doc.unreadable ?? null,
      history:
        doc.history ??
        (version > 0 ? [{ version, written_at: NOW, written_by: "seed", deleted: false }] : []),
      snapshots,
    });
  }

  const double: SettingsDouble = {
    writes: [],
    rollbacks: [],
    failNext: null,
    external(scope, name, body) {
      const doc = docs.get(keyOf(scope, name));
      if (doc === undefined) throw new Error(`no seeded document ${scope}/${name}`);
      doc.version += 1;
      doc.body = body;
      doc.unreadable = null;
      doc.snapshots.set(doc.version, body);
      doc.history = [{ version: doc.version, written_at: NOW, written_by: "elsewhere", deleted: false }, ...doc.history];
    },
    async handle(path, init) {
      const [pathname, query = ""] = path.split("?");
      const segments = pathname
        .split("/")
        .filter((segment) => segment !== "")
        .map(decodeURIComponent);
      const method = init?.method ?? "GET";
      const payload: unknown = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;

      if (segments.length === 1) {
        const wanted = new URLSearchParams(query).get("scope");
        const rows: SettingIndexRow[] = [];
        for (const doc of docs.values()) {
          if (wanted !== null && doc.seed.scope !== wanted) continue;
          rows.push({
            scope: doc.seed.scope,
            name: doc.seed.name,
            kind: doc.seed.scope === "machine" ? "machine" : "project",
            takes_effect: doc.seed.takes_effect ?? "when the daemon restarts",
            present: doc.version > 0,
            unreadable: doc.unreadable !== null,
            version: doc.version,
          });
        }
        return rows;
      }

      const [, scope, name, tail, target] = segments;
      const doc = docs.get(keyOf(scope, name));
      if (doc === undefined) {
        throw method === "GET" ? refuse(404, { refusal: "unknown_document" }) : refuse(403, { refusal: "not_ours" });
      }

      if (tail === undefined && method === "GET") {
        if (doc.unreadable !== null) {
          throw refuse(422, { refusal: "unreadable", detail: doc.unreadable, version: doc.version });
        }
        return { body: doc.body, version: doc.version, schema: doc.seed.schema };
      }
      if (tail === "history" && method === "GET") {
        return [...doc.history].sort((a, b) => b.version - a.version);
      }

      if (double.failNext !== null) {
        const failure = double.failNext;
        double.failNext = null;
        throw refuse(failure.status, failure.body);
      }

      if (tail === undefined && method === "PUT") {
        const { body, version } = payload as { body: unknown; version: number };
        // The body before the version, as the store does.
        const verdict = doc.seed.validate?.(body) ?? null;
        if (verdict !== null) {
          throw refuse(422, { refusal: "invalid", ...(verdict.field !== undefined ? { field: verdict.field } : {}), detail: verdict.message });
        }
        if (version !== doc.version) throw refuse(409, { refusal: "stale", current: doc.version });
        doc.version += 1;
        doc.body = body;
        doc.unreadable = null;
        doc.snapshots.set(doc.version, body);
        doc.history = [{ version: doc.version, written_at: NOW, written_by: "test", deleted: false }, ...doc.history];
        double.writes.push({ scope, name, body, version });
        return { version: doc.version };
      }

      if (tail === "rollback" && method === "POST") {
        const wanted = Number(target);
        const { version: current } = payload as { version: number };
        if (current !== doc.version) throw refuse(409, { refusal: "stale", current: doc.version });
        if (!doc.snapshots.has(wanted)) throw refuse(404, { refusal: "no_such_version" });
        const restored = doc.snapshots.get(wanted);
        doc.version += 1;
        doc.body = restored;
        doc.unreadable = null;
        doc.snapshots.set(doc.version, restored);
        doc.history = [{ version: doc.version, written_at: NOW, written_by: "test", deleted: false }, ...doc.history];
        double.rollbacks.push({ scope, name, target: wanted, current });
        return { version: doc.version };
      }

      throw refuse(404, { refusal: "not_found", detail: `${method} ${path}` });
    },
  };
  return double;
}
