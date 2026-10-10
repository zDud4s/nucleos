/** Wire shapes between the panel (in the page's isolated world) and the sidecar. */

export type Role = "agent" | "person";
export type Mode = "agent" | "human";

export type Incoming =
  | { v: 1; kind: "message"; role: Role; text: string; ts: string }
  | { v: 1; kind: "state"; mode: Mode; host: string; collapsed: boolean }
  | { v: 1; kind: "ask_wheel"; reason: string }
  | { v: 1; kind: "ask_keep"; hosts: string[] }
  | { v: 1; kind: "delivery"; id: string; ok: boolean };

export type Outgoing =
  | { v: 1; kind: "say"; id: string; text: string }
  | { v: 1; kind: "take_wheel" }
  | { v: 1; kind: "give_back"; note: string }
  | { v: 1; kind: "collapse"; collapsed: boolean }
  | { v: 1; kind: "keep"; keep: boolean; writable: boolean };

export interface PanelMessage {
  key: string;
  role: Role;
  text: string;
  ts: string;
  /** The id of a `say` still waiting on its delivery receipt. */
  id?: string;
  failed?: boolean;
}

export interface PanelState {
  messages: PanelMessage[];
  mode: Mode;
  host: string;
  collapsed: boolean;
  askWheel: string | null;
  askKeep: string[] | null;
}

export function initialState(): PanelState {
  return {
    messages: [],
    mode: "agent",
    host: "",
    collapsed: false,
    askWheel: null,
    askKeep: null,
  };
}

/** Appends the person's own line before the sidecar has confirmed it. */
export function pendingSay(state: PanelState, id: string, text: string, ts: string): PanelState {
  const message: PanelMessage = { key: `say:${id}`, role: "person", text, ts, id };
  return { ...state, messages: [...state.messages, message] };
}

/** Clears the failed mark while a retry is in flight. */
export function retrying(state: PanelState, id: string): PanelState {
  return {
    ...state,
    messages: state.messages.map((m) => (m.id === id ? { ...m, failed: false } : m)),
  };
}

export function clearKeep(state: PanelState): PanelState {
  return { ...state, askKeep: null };
}

export function reduce(state: PanelState, incoming: Incoming): PanelState {
  switch (incoming.kind) {
    case "message": {
      const key = `${incoming.role}|${incoming.ts}|${incoming.text}`;
      if (state.messages.some((m) => m.key === key)) return state;
      const message: PanelMessage = {
        key,
        role: incoming.role,
        text: incoming.text,
        ts: incoming.ts,
      };
      if (incoming.role === "person") {
        // Core mirrors the person's own words back; adopt the oldest unmatched
        // local line with the same text instead of showing it twice.
        const at = state.messages.findIndex(
          (m) => m.key.startsWith("say:") && m.text === incoming.text,
        );
        if (at >= 0) {
          const messages = state.messages.slice();
          messages[at] = { ...messages[at], key, ts: incoming.ts };
          return { ...state, messages };
        }
      }
      return { ...state, messages: [...state.messages, message] };
    }
    case "state":
      return {
        ...state,
        mode: incoming.mode,
        host: incoming.host,
        collapsed: incoming.collapsed,
        askWheel: incoming.mode === "human" ? null : state.askWheel,
      };
    case "ask_wheel":
      return { ...state, askWheel: incoming.reason };
    case "ask_keep":
      return { ...state, askKeep: incoming.hosts };
    case "delivery": {
      if (!state.messages.some((m) => m.id === incoming.id)) return state;
      return {
        ...state,
        messages: state.messages.map((m) =>
          m.id === incoming.id ? { ...m, failed: !incoming.ok } : m,
        ),
      };
    }
  }
}

export function say(id: string, text: string): Outgoing {
  return { v: 1, kind: "say", id, text };
}
export function takeWheel(): Outgoing {
  return { v: 1, kind: "take_wheel" };
}
export function giveBack(note: string): Outgoing {
  return { v: 1, kind: "give_back", note };
}
export function collapse(collapsed: boolean): Outgoing {
  return { v: 1, kind: "collapse", collapsed };
}
export function keep(keepIt: boolean, writable: boolean): Outgoing {
  return { v: 1, kind: "keep", keep: keepIt, writable };
}
