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
  verdict: "continues" | "closes";
  elapsedMs: number;
};

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
  const segments = hasText ? [...state.segments, segment.text] : state.segments;

  if (segment.verdict === "closes") {
    return {
      state: EMPTY_TURN,
      signal: { type: "deliver", text: segments.join(" ") },
    };
  }

  return {
    state: {
      segments,
      speechMs: state.speechMs + (hasText ? segment.elapsedMs : 0),
      idleMs: hasText ? 0 : state.idleMs + segment.elapsedMs,
      pendingDiscard: state.pendingDiscard,
    },
    signal: null,
  };
}
