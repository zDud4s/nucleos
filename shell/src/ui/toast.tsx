import {
  Children,
  isValidElement,
  useEffect,
  useRef,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { CircleAlertIcon, CircleCheckIcon, InfoIcon, LoaderCircleIcon, XIcon } from "lucide-react";

/**
 * Toasts: a fixed-position stack that never takes part in page layout.
 *
 * The API follows coss/ui's `toastManager` (`add`, `close`, `promise`) but is a small
 * external store rather than Base UI's Toast, which is not a dependency of the shell.
 * Errors persist longer than other toasts; a loading toast persists until it settles.
 */

export type ToastType = "error" | "success" | "info" | "loading";

export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface ToastOptions {
  title?: ReactNode;
  description?: ReactNode;
  type?: ToastType;
  /** Milliseconds before it closes itself. 0 keeps it until dismissed. */
  timeout?: number;
  /** Same key shown (or shown moments ago) means this call is a no-op. */
  dedupeKey?: string;
  action?: ToastAction;
}

export interface ToastItem extends ToastOptions {
  id: string;
  type: ToastType;
}

export const TOAST_MS = 5000;
export const TOAST_ERROR_MS = 12000;
/** A deduplicated toast stays suppressed this long after it was shown. */
export const TOAST_DEDUPE_MS = 15000;

type Resolvable<T, A extends unknown[]> = T | ((...args: A) => T);

export interface ToastPromiseOptions<T> {
  loading: ToastOptions;
  success: Resolvable<ToastOptions, [T]>;
  error: Resolvable<ToastOptions, [unknown]>;
}

let items: ToastItem[] = [];
let counter = 0;
const listeners = new Set<() => void>();
const timers = new Map<string, ReturnType<typeof setTimeout>>();
const shownAt = new Map<string, number>();

function emit() {
  for (const l of listeners) l();
}

function arm(item: ToastItem) {
  const prior = timers.get(item.id);
  if (prior !== undefined) clearTimeout(prior);
  timers.delete(item.id);
  const ms = item.timeout ?? (item.type === "error" ? TOAST_ERROR_MS : item.type === "loading" ? 0 : TOAST_MS);
  if (ms > 0) timers.set(item.id, setTimeout(() => toastManager.close(item.id), ms));
}

export const toastManager = {
  /** Show a toast and return its id (the id of the live twin when deduplicated). */
  add(options: ToastOptions): string {
    const key = options.dedupeKey;
    if (key !== undefined) {
      const live = items.find((i) => i.dedupeKey === key);
      if (live) return live.id;
      const last = shownAt.get(key);
      if (last !== undefined && Date.now() - last < TOAST_DEDUPE_MS) return "";
      shownAt.set(key, Date.now());
    }
    const item: ToastItem = { ...options, type: options.type ?? "info", id: `toast-${++counter}` };
    items = [...items, item];
    arm(item);
    emit();
    return item.id;
  },
  /** Change a live toast in place (what `promise` does when it settles). */
  update(id: string, options: ToastOptions) {
    let next: ToastItem | undefined;
    items = items.map((i) => {
      if (i.id !== id) return i;
      next = { ...i, ...options, type: options.type ?? i.type, id };
      return next;
    });
    if (next) arm(next);
    emit();
  },
  close(id: string) {
    const t = timers.get(id);
    if (t !== undefined) clearTimeout(t);
    timers.delete(id);
    if (!items.some((i) => i.id === id)) return;
    items = items.filter((i) => i.id !== id);
    emit();
  },
  /** Loading toast that becomes a success or error toast. Resolves/rejects as `p` does. */
  promise<T>(p: Promise<T>, opts: ToastPromiseOptions<T>): Promise<T> {
    const id = toastManager.add({ ...opts.loading, type: "loading" });
    p.then(
      (data) => {
        const o = typeof opts.success === "function" ? opts.success(data) : opts.success;
        toastManager.update(id, { timeout: undefined, action: undefined, ...o, type: "success" });
      },
      (err: unknown) => {
        const o = typeof opts.error === "function" ? opts.error(err) : opts.error;
        toastManager.update(id, { timeout: undefined, action: undefined, ...o, type: "error" });
      },
    );
    return p;
  },
  /** Drop everything, and forget what was deduplicated. */
  clear() {
    for (const t of timers.values()) clearTimeout(t);
    timers.clear();
    shownAt.clear();
    items = [];
    emit();
  },
  subscribe(l: () => void) {
    listeners.add(l);
    return () => void listeners.delete(l);
  },
  snapshot(): ToastItem[] {
    return items;
  },
};

/** Plain text of a React node, for a toast title taken from banner children. */
export function textOf(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return textOf(Children.toArray(node.props.children));
  return "";
}

/**
 * Raise a toast when a component that used to draw a banner mounts (or its message changes).
 * `key` defaults to the message, so a refetch that fails the same way does not toast again.
 */
export function useToastOnce(options: ToastOptions & { key?: string }) {
  const { key, ...rest } = options;
  const latest = useRef(rest);
  latest.current = rest;
  const id = key ?? `${rest.type ?? "info"}:${textOf(rest.title)}:${textOf(rest.description)}`;
  useEffect(() => {
    toastManager.add({ ...latest.current, dedupeKey: id });
  }, [id]);
}

const ICONS = {
  error: CircleAlertIcon,
  success: CircleCheckIcon,
  info: InfoIcon,
  loading: LoaderCircleIcon,
} as const;

function ToastCard({ item }: { item: ToastItem }) {
  const Icon = ICONS[item.type];
  return (
    <div
      className={`ui-toast ui-toast-${item.type}`}
      role={item.type === "error" ? "alert" : "status"}
      data-toast-type={item.type}
    >
      <div className="ui-toast-body">
        <Icon className="ui-toast-icon" aria-hidden="true" />
        <div className="ui-toast-text">
          {item.title ? <div className="ui-toast-title">{item.title}</div> : null}
          {item.description ? <div className="ui-toast-description">{item.description}</div> : null}
          {item.action ? (
            <button
              type="button"
              className="ui-toast-action"
              onClick={() => {
                item.action?.onClick();
                toastManager.close(item.id);
              }}
            >
              {item.action.label}
            </button>
          ) : null}
        </div>
      </div>
      <button
        type="button"
        className="ui-toast-close"
        aria-label="Dismiss notification"
        onClick={() => toastManager.close(item.id)}
      >
        <XIcon aria-hidden="true" />
      </button>
    </div>
  );
}

/** The stack itself. Fixed to the corner, so it cannot move the page. */
export function Toaster() {
  const list = useSyncExternalStore(toastManager.subscribe, toastManager.snapshot, toastManager.snapshot);
  return (
    <div className="ui-toaster" aria-live="polite">
      {list.map((t) => (
        <ToastCard key={t.id} item={t} />
      ))}
    </div>
  );
}

/** Mount once, around the app; renders the stack after its children. */
export function ToastProvider({ children }: { children?: ReactNode }) {
  return (
    <>
      {children}
      <Toaster />
    </>
  );
}
