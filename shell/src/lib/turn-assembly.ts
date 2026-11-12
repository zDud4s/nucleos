export type TurnState = {
  segments: string[];
  speechMs: number;
  idleMs: number;
  pendingDiscard: boolean;
};

export type TurnSignal =
  | { type: "deliver"; text: string }
  | { type: "discard" }
  | { type: "confirmDiscard" }
  | { type: "abandon" };

export type Segment = {
  text: string;
  verdict: "continues" | "closes" | "discards" | "confirms";
  elapsedMs: number;
};

const MAX_TURN_MS = 5 * 60_000;
const IDLE_LIMIT_MS = 10 * 60_000;

export const EMPTY_TURN: TurnState = {
  segments: [],
  speechMs: 0,
  idleMs: 0,
  pendingDiscard: false,
};

export function onSegment(
  state: TurnState,
  segment: Segment,
): { state: TurnState; signal: TurnSignal | null } {
  const hasText = segment.text.length > 0;
  const isDiscardRequest = segment.verdict === "discards";
  const segments = hasText && !isDiscardRequest ? [...state.segments, segment.text] : state.segments;
  const nextState: TurnState = {
    segments,
    speechMs: state.speechMs + (hasText ? segment.elapsedMs : 0),
    idleMs: 0,
    pendingDiscard: isDiscardRequest,
  };

  if (segment.verdict === "confirms" && state.pendingDiscard) {
    return {
      state: EMPTY_TURN,
      signal: { type: "discard" },
    };
  }

  if (segment.verdict === "closes") {
    return {
      state: EMPTY_TURN,
      signal: { type: "deliver", text: segments.join(" ") },
    };
  }

  if (nextState.speechMs >= MAX_TURN_MS && !nextState.pendingDiscard) {
    return {
      state: EMPTY_TURN,
      signal: { type: "deliver", text: segments.join(" ") },
    };
  }

  if (isDiscardRequest) {
    return {
      state: nextState,
      signal: { type: "confirmDiscard" },
    };
  }

  return {
    state: nextState,
    signal: null,
  };
}

export function onIdle(
  state: TurnState,
  elapsedMs: number,
): { state: TurnState; signal: TurnSignal | null } {
  const idleMs = state.idleMs + elapsedMs;

  if (idleMs >= IDLE_LIMIT_MS) {
    return {
      state: EMPTY_TURN,
      signal: { type: "abandon" },
    };
  }

  return {
    state: { ...state, idleMs },
    signal: null,
  };
}
