import { RING_ROWS } from "./limits";
import type { ConnectionState, FleetEvent, FleetNode, NodeMetrics, NodeState, TermLine } from "./types";

export interface NodeEntry {
  readonly node: FleetNode;
  readonly lines: readonly TermLine[];
  readonly metrics: NodeMetrics | null;
}
export interface Transition {
  node: FleetNode;
  from: NodeState;
}
type Listener = () => void;

/**
 * Everything about a node that decides where it sits in the forest, as one string. A `node`
 * event whose key is unchanged (state busy/idle, label, role, model, capabilities, pinned) is
 * "state only" and never reaches `layoutForest`; `done`/`lost` are part of the key because a node
 * entering or leaving the done group moves its siblings.
 */
export function layoutKey(n: FleetNode): string {
  const finished = n.state === "done" ? "d" : n.state === "lost" ? "l" : "a";
  return [n.parent_id ?? "", n.project ?? "", n.started_at, finished, n.done_omitted].join("|");
}

const sameJson = (a: unknown, b: unknown): boolean => a === b || JSON.stringify(a) === JSON.stringify(b);

/**
 * The live tree, outside React.
 *
 * Three subscription channels, deliberately separate: `subscribeNode(id)` (lines, metrics, any
 * change to that node), `subscribeLayout` (a version that moves only when structure moves) and the
 * small ones (connection, last-active, transitions, line batches). Entries are replaced, never
 * mutated, so a `useSyncExternalStore` snapshot is a stable reference until its node changes.
 */
export class FleetStore {
  private entries = new Map<string, NodeEntry>();
  private keys = new Map<string, string>();
  private layoutVersion = 0;
  private received = false;
  private connection: ConnectionState = "connecting";
  private failedReconnects = 0;
  private active: string | null = null;

  private nodeListeners = new Map<string, Set<Listener>>();
  private layoutListeners = new Set<Listener>();
  private connectionListeners = new Set<Listener>();
  private activeListeners = new Set<Listener>();
  private transitionListeners = new Set<(t: Transition) => void>();
  private lineListeners = new Map<string, Set<(lines: readonly TermLine[]) => void>>();

  /* ------------------------------------------------------------------ reads -- */

  getEntry = (id: string): NodeEntry | undefined => this.entries.get(id);
  getLayoutVersion = (): number => this.layoutVersion;
  getConnection = (): ConnectionState => this.connection;
  /** Reconnects in a row that have not reached a snapshot yet (0 while live). */
  getFailedReconnects = (): number => this.failedReconnects;
  getActive = (): string | null => this.active;
  hasSnapshot = (): boolean => this.received;
  nodes = (): FleetNode[] => [...this.entries.values()].map((entry) => entry.node);

  /* ---------------------------------------------------------- subscriptions -- */

  subscribeNode = (id: string, cb: Listener): (() => void) => this.add(this.nodeListeners, id, cb);
  subscribeLayout = (cb: Listener): (() => void) => this.addTo(this.layoutListeners, cb);
  subscribeConnection = (cb: Listener): (() => void) => this.addTo(this.connectionListeners, cb);
  subscribeActive = (cb: Listener): (() => void) => this.addTo(this.activeListeners, cb);
  subscribeTransitions = (cb: (t: Transition) => void): (() => void) => this.addTo(this.transitionListeners, cb);
  /** New lines of one node as they arrive (the transcript panel's live tail). */
  onLines = (id: string, cb: (lines: readonly TermLine[]) => void): (() => void) => this.add(this.lineListeners, id, cb);

  private addTo<T>(set: Set<T>, cb: T): () => void {
    set.add(cb);
    return () => void set.delete(cb);
  }
  private add<T>(map: Map<string, Set<T>>, id: string, cb: T): () => void {
    const set = map.get(id) ?? new Set<T>();
    set.add(cb);
    map.set(id, set);
    return () => void set.delete(cb);
  }

  /* ----------------------------------------------------------------- writes -- */

  setConnection(state: ConnectionState): void {
    // The stream says `reconnecting` once per failed attempt, so a repeat of the state is still news.
    const failed = state === "reconnecting" ? this.failedReconnects + 1 : state === "live" ? 0 : this.failedReconnects;
    if (state === this.connection && failed === this.failedReconnects) return;
    this.connection = state;
    this.failedReconnects = failed;
    this.connectionListeners.forEach((cb) => cb());
  }

  apply(event: FleetEvent): void {
    switch (event.t) {
      case "snapshot":
        this.replace(event);
        break;
      case "node":
        this.upsert(event.node);
        break;
      case "metrics":
        for (const metrics of event.metrics) this.setMetrics(metrics);
        break;
      case "lines":
        this.append(event.node_id, event.lines);
        break;
      case "removed":
        this.drop(event.node_id);
        break;
      case "heartbeat":
        break;
    }
  }

  private notifyNode(id: string): void {
    this.nodeListeners.get(id)?.forEach((cb) => cb());
  }
  private bumpLayout(): void {
    this.layoutVersion += 1;
    this.layoutListeners.forEach((cb) => cb());
  }

  private replace(event: Extract<FleetEvent, { t: "snapshot" }>): void {
    const before = this.entries;
    const beforeKeys = this.keys;
    const metrics = new Map(event.metrics.map((m) => [m.node_id, m]));
    const next = new Map<string, NodeEntry>();
    const keys = new Map<string, string>();
    for (const node of event.nodes) {
      const old = before.get(node.id);
      const lines = (event.lines[node.id] ?? []).slice(-RING_ROWS);
      const m = metrics.get(node.id) ?? null;
      const same = old !== undefined && sameJson(old.node, node) && sameJson(old.lines, lines) && sameJson(old.metrics, m);
      next.set(node.id, same ? old : { node, lines, metrics: m });
      keys.set(node.id, layoutKey(node));
    }
    const touched = new Set<string>([...before.keys(), ...next.keys()]);
    const structural =
      !this.received || beforeKeys.size !== keys.size || [...keys].some(([id, key]) => beforeKeys.get(id) !== key);
    this.entries = next;
    this.keys = keys;
    this.received = true;
    for (const id of touched) if (before.get(id) !== next.get(id)) this.notifyNode(id);
    if (this.active !== null && !next.has(this.active)) {
      this.active = null;
      this.activeListeners.forEach((cb) => cb());
    }
    if (structural) this.bumpLayout();
  }

  private upsert(node: FleetNode): void {
    const old = this.entries.get(node.id);
    if (old !== undefined && sameJson(old.node, node)) return;
    this.entries.set(node.id, { node, lines: old?.lines ?? [], metrics: old?.metrics ?? null });
    const key = layoutKey(node);
    const structural = this.keys.get(node.id) !== key;
    this.keys.set(node.id, key);
    this.notifyNode(node.id);
    if (old !== undefined && old.node.state !== node.state) {
      this.transitionListeners.forEach((cb) => cb({ node, from: old.node.state }));
    }
    if (structural) this.bumpLayout();
  }

  private setMetrics(metrics: NodeMetrics): void {
    const old = this.entries.get(metrics.node_id);
    if (old === undefined || sameJson(old.metrics, metrics)) return;
    this.entries.set(metrics.node_id, { ...old, metrics });
    this.notifyNode(metrics.node_id);
  }

  private append(id: string, lines: TermLine[]): void {
    const old = this.entries.get(id);
    if (old === undefined || lines.length === 0) return;
    this.entries.set(id, { ...old, lines: [...old.lines, ...lines].slice(-RING_ROWS) });
    this.notifyNode(id);
    this.lineListeners.get(id)?.forEach((cb) => cb(lines));
    if (this.active !== id) {
      this.active = id;
      this.activeListeners.forEach((cb) => cb());
    }
  }

  private drop(rootId: string): void {
    const gone = [...this.entries.values()]
      .filter((entry) => entry.node.id === rootId || entry.node.root_id === rootId)
      .map((entry) => entry.node.id);
    if (gone.length === 0) return;
    for (const id of gone) {
      this.entries.delete(id);
      this.keys.delete(id);
    }
    if (this.active !== null && gone.includes(this.active)) {
      this.active = null;
      this.activeListeners.forEach((cb) => cb());
    }
    gone.forEach((id) => this.notifyNode(id));
    this.bumpLayout();
  }
}
