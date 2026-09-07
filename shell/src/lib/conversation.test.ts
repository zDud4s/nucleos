import { describe, expect, it } from "vitest";

import {
  ConversationEvent,
  ConversationPhase,
  micIsOpen,
  onConversationEvent,
} from "./conversation";

const EVENTS: ConversationEvent["type"][] = [
  "toggled",
  "speechStarted",
  "speechEnded",
  "turnClosed",
  "turnDiscarded",
  "turnRefused",
  "answerStarted",
  "answerEnded",
];

const PHASES: ConversationPhase[] = [
  "off",
  "listening",
  "hearing",
  "thinking",
  "speaking",
];

/** Runs a script of events from `off` and returns where it ended up. */
function drive(events: ConversationEvent["type"][]): {
  phase: ConversationPhase;
  actions: string[];
} {
  let phase: ConversationPhase = "off";
  const actions: string[] = [];
  for (const type of events) {
    const next = onConversationEvent(phase, { type } as ConversationEvent);
    phase = next.phase;
    actions.push(next.action);
  }
  return { phase, actions };
}

describe("the conversation mode", () => {
  it("runs a whole turn: open, hear, send, speak, and back to listening", () => {
    const { phase, actions } = drive([
      "toggled",
      "speechStarted",
      "speechEnded",
      "turnClosed",
      "answerStarted",
      "answerEnded",
    ]);

    expect(actions).toEqual([
      "openMic",
      "startRecording",
      "transcribeSegment",
      "sendTurn",
      "nothing",
      "nothing",
    ]);
    expect(phase).toBe("listening");
  });

  /**
   * The reason the mode exists. Cutting in has to stop the sound in the SAME transition that starts
   * recording — a two-step version leaves a window where the answer is still audible and the
   * microphone is already recording it.
   */
  it("treats talking over the answer as barge-in, in one decision", () => {
    const barge = onConversationEvent("speaking", { type: "speechStarted" });

    expect(barge).toEqual({ phase: "hearing", action: "stopPlaybackAndRecord" });
  });

  /**
   * Rule 3, and the opposite call from the dictation hotkey. Two dictations race into one window;
   * two questions merely queue, and the núcleo already knows how to queue one.
   */
  it("keeps a sentence said while the agent is still thinking", () => {
    const interjection = onConversationEvent("thinking", { type: "speechStarted" });

    expect(interjection).toEqual({ phase: "hearing", action: "startRecording" });
  });

  it("goes back to listening when the turn is refused, rather than waiting for audio that will never come", () => {
    expect(onConversationEvent("thinking", { type: "turnRefused" })).toEqual({
      phase: "listening",
      action: "nothing",
    });
  });

  it("goes back to listening when a turn produced no speech at all", () => {
    expect(onConversationEvent("thinking", { type: "answerEnded" })).toEqual({
      phase: "listening",
      action: "nothing",
    });
  });

  /**
   * Rule 1. Anything less than "from every phase" can strand an open microphone, and this pillar
   * exists because of privacy.
   */
  it.each(PHASES.filter((phase) => phase !== "off"))(
    "leaves the mode from %s",
    (phase) => {
      expect(onConversationEvent(phase, { type: "toggled" }).phase).toBe("off");
    },
  );

  it("stops the answer on the way out, but only when there was one playing", () => {
    expect(onConversationEvent("speaking", { type: "toggled" }).action).toBe(
      "stopPlaybackAndCloseMic",
    );
    expect(onConversationEvent("thinking", { type: "toggled" }).action).toBe("closeMic");
    expect(onConversationEvent("listening", { type: "toggled" }).action).toBe("closeMic");
  });

  /**
   * The gate can fire once more after the microphone is asked to close. A mode that restarted itself
   * on that event is a mode that cannot be switched off.
   */
  it.each(EVENTS.filter((type) => type !== "toggled"))(
    "ignores a stray %s once it is off",
    (type) => {
      expect(onConversationEvent("off", { type } as ConversationEvent)).toEqual({
        phase: "off",
        action: "nothing",
      });
    },
  );

  /**
   * Totality is the property, not a nicety: a phase that answered some events and not others would
   * be a phase a key press could get stuck in, with an open microphone.
   */
  it.each(PHASES)("answers every event from %s", (phase) => {
    for (const type of EVENTS) {
      const next = onConversationEvent(phase, { type } as ConversationEvent);
      expect(PHASES).toContain(next.phase);
      expect(next.action).toBeTypeOf("string");
    }
  });

  it("holds an answer that lands mid-sentence instead of playing over the person", () => {
    expect(onConversationEvent("hearing", { type: "answerStarted" })).toEqual({
      phase: "hearing",
      action: "nothing",
    });
  });

  it("silence ends a segment and leaves the conversation hearing", () => {
    expect(onConversationEvent("hearing", { type: "speechEnded" })).toEqual({
      phase: "hearing",
      action: "transcribeSegment",
    });
  });

  it("speech starting again inside a turn records the next segment", () => {
    // Without this, segment 1 records and every segment after it is dropped: `startRecording` is the
    // only thing that seeds the recording buffer, and `onAudioFrame` throws away frames while it is
    // null. This is the transition the whole feature stands on.
    expect(onConversationEvent("hearing", { type: "speechStarted" })).toEqual({
      phase: "hearing",
      action: "startRecording",
    });
  });

  it("a closed turn is what sends it", () => {
    expect(onConversationEvent("hearing", { type: "turnClosed" })).toEqual({
      phase: "thinking",
      action: "sendTurn",
    });
  });

  it("a discarded turn goes back to listening without sending", () => {
    expect(onConversationEvent("hearing", { type: "turnDiscarded" })).toEqual({
      phase: "listening",
      action: "nothing",
    });
  });

  it("segmentation does not start while the answer is playing", () => {
    // The AEC is unproven (spec §7). Speech STARTING is still barge-in; a segment must not open.
    expect(onConversationEvent("speaking", { type: "speechEnded" })).toEqual({
      phase: "speaking",
      action: "nothing",
    });
    expect(onConversationEvent("speaking", { type: "speechStarted" })).toEqual({
      phase: "hearing",
      action: "stopPlaybackAndRecord",
    });
  });

  it("knows when the microphone is open, so the window can say so", () => {
    expect(micIsOpen("off")).toBe(false);
    for (const phase of PHASES.filter((p) => p !== "off")) {
      expect(micIsOpen(phase)).toBe(true);
    }
  });

  /** A double press is a press and a press: in, then straight back out. */
  it("returns to off on a second toggle", () => {
    expect(drive(["toggled", "toggled"]).phase).toBe("off");
  });
});
