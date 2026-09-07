/**
 * Where a hands-free conversation is, and what each thing that happens to it means.
 *
 * **Why this is here and not in `shell/src-tauri/src/voice.rs`.** That file holds the dictation
 * pillar's pure decisions, and by its own header that is where "every subtle decision" belongs — so
 * this is the first place a reader will look for the rule below, and not finding it is a cost paid
 * deliberately.
 *
 * The reason is barge-in. Cutting in while the agent is talking is not an error path here; it is the
 * whole point of a hands-free mode, and stopping the audio has to feel instant. The events that drive
 * this machine (speech started, speech ended, the answer ran out) all originate in the webview, and
 * every action it produces (open the microphone, stop the playback) is carried out in the webview —
 * so putting the decision in the other process would mean an IPC round trip, JSON-serialised, between
 * hearing the person start talking and stopping the sound that is talking over them. `audio.ts`
 * records the same boundary for the same reason: whoever holds the samples should be the one deciding
 * about them.
 *
 * What DOES stay in Rust is the hotkey, because a global chord needs the desktop session. It arrives
 * here as one `toggled` event and nothing else.
 *
 * The machine is total: every phase answers every event. A hands-free mode that can reach a state
 * where a key press does nothing is a mode that traps somebody with an open microphone.
 */

export type ConversationPhase =
  /** Not in conversation mode. The microphone is closed. */
  | "off"
  /** Microphone open, nobody talking yet. */
  | "listening"
  /** Somebody is talking, and it is being recorded. */
  | "hearing"
  /** The recording is with the núcleo and no audio has come back. */
  | "thinking"
  /** The answer is playing — with the microphone still open, which is what barge-in needs. */
  | "speaking";

export type ConversationEvent =
  /** The hotkey, or the tab's own button. */
  | { type: "toggled" }
  /** The gate in `vad.ts` decided somebody started talking. */
  | { type: "speechStarted" }
  /** ...and stopped. */
  | { type: "speechEnded" }
  /** The accumulated segments form a complete turn. */
  | { type: "turnClosed" }
  /** The accumulated segments were discarded. */
  | { type: "turnDiscarded" }
  /** The núcleo would not take the turn — a kill switch, a chat with no model. */
  | { type: "turnRefused" }
  /** The first unit of the answer is ready to play. */
  | { type: "answerStarted" }
  /** The answer ran out: the speech endpoint said 404. */
  | { type: "answerEnded" };

export type ConversationAction =
  | "openMic"
  | "closeMic"
  | "startRecording"
  | "transcribeSegment"
  | "sendTurn"
  /** Barge-in: stop the answer mid-word and start recording what is being said over it. */
  | "stopPlaybackAndRecord"
  /** Leaving the mode while it was talking. */
  | "stopPlaybackAndCloseMic"
  | "nothing";

export interface Transition {
  phase: ConversationPhase;
  action: ConversationAction;
}

/**
 * What one event means, given where the conversation already is.
 *
 * Four rules carry the design, and each exists against a specific way of getting it wrong:
 *
 * 1. **`toggled` always leaves, from every phase.** A mode whose exit depends on being in the right
 *    state is a mode that can strand an open microphone, which is the one failure this pillar exists
 *    to make impossible.
 * 2. **Speech starting while the agent talks is barge-in, not noise.** It stops the playback in the
 *    same transition that starts recording — one decision, so there is no window in which the answer
 *    is still audible and the person is already being recorded over it.
 * 3. **Speech starting while the agent THINKS is kept, not dropped.** The núcleo queues it
 *    (`send_or_queue`), so the cost of accepting is a wait and the cost of refusing is a lost
 *    sentence. This is the opposite call from the dictation hotkey, which absorbs presses — and the
 *    reason differs too: two dictations race into one window, while two questions merely queue.
 * 4. **One turn at a time.** Sending a new turn abandons whatever answer was still arriving. That is
 *    what somebody who interrupts means, and tracking two would mean deciding which one gets the
 *    speaker.
 *
 * A stray event in `off` changes nothing. The gate can fire once more after the microphone closes,
 * and a mode that restarted itself on that would be a mode that cannot be switched off.
 */
export function onConversationEvent(
  phase: ConversationPhase,
  event: ConversationEvent,
): Transition {
  if (event.type === "toggled") {
    if (phase === "off") return { phase: "listening", action: "openMic" };
    return {
      phase: "off",
      action: phase === "speaking" ? "stopPlaybackAndCloseMic" : "closeMic",
    };
  }

  switch (phase) {
    case "off":
      return { phase: "off", action: "nothing" };

    case "listening":
      return event.type === "speechStarted"
        ? { phase: "hearing", action: "startRecording" }
        : { phase: "listening", action: "nothing" };

    case "hearing":
      // An answer that lands while somebody is mid-sentence is held rather than played over them.
      // It is not lost: the turn it belongs to is abandoned by rule 4 as soon as this one is sent.
      switch (event.type) {
        case "speechStarted":
          return { phase: "hearing", action: "startRecording" };
        case "speechEnded":
          return { phase: "hearing", action: "transcribeSegment" };
        case "turnClosed":
          return { phase: "thinking", action: "sendTurn" };
        case "turnDiscarded":
          return { phase: "listening", action: "nothing" };
        default:
          return { phase: "hearing", action: "nothing" };
      }

    case "thinking":
      switch (event.type) {
        case "answerStarted":
          return { phase: "speaking", action: "nothing" };
        case "speechStarted":
          return { phase: "hearing", action: "startRecording" };
        // A turn that produced no speech at all, and a turn that was refused, both end the same way:
        // back to listening. The difference is worth showing on screen and is not worth a phase —
        // the microphone's state is identical either way.
        case "answerEnded":
        case "turnRefused":
          return { phase: "listening", action: "nothing" };
        default:
          return { phase: "thinking", action: "nothing" };
      }

    case "speaking":
      switch (event.type) {
        case "speechStarted":
          return { phase: "hearing", action: "stopPlaybackAndRecord" };
        case "answerEnded":
          return { phase: "listening", action: "nothing" };
        default:
          return { phase: "speaking", action: "nothing" };
      }
  }
}

/** Whether the microphone should be open in this phase, for anything that needs to show it. */
export function micIsOpen(phase: ConversationPhase): boolean {
  return phase !== "off";
}
