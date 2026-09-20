/**
 * Talking into a box that has no conversation behind it yet.
 *
 * **Not the hands-free conversation, and the difference is not a preference.** `useVoiceConversation`
 * runs a loop: it hears a sentence, sends it as a turn, and plays the answer back. Both ends of that
 * need a conversation to exist — `sendTurn` refuses outright when it has no chat id, saying so in as
 * many words. The front door and the pick-up box have no conversation by definition; one is where a
 * conversation is created and the other is an editor session that has not been carried on here yet.
 *
 * So this does the other thing: it turns speech into TEXT and hands it back, and the box it fills is
 * sent the ordinary way. Three things follow from that, and all three are why it is the right shape
 * rather than a lesser one:
 *
 * - A misheard sentence is visible and editable BEFORE anything is sent. On the front door the first
 *   sentence is what opens the conversation, and a conversation is a billed row that is archived
 *   rather than deleted — so opening one on the wrong words is not undoable.
 * - Nothing has to be created to press the button. A mic that opened an empty conversation just to
 *   have somewhere to send audio would leave one behind every time somebody pressed it and said
 *   nothing.
 * - It is what the box is for. You are standing in front of a thing you are about to type into.
 *
 * The route is the same one the global dictation hotkey uses (`POST /voice/capture?kind=dictation`),
 * which means what is said here lands in the daemon's dictation list and is kept for
 * `retain_dictations_days` like any other. That is a consequence worth knowing rather than a
 * decision taken here: it is one dictation route, and a second kind that meant "the same, but do not
 * write it down" would be a second retention policy for the same sentence.
 */

import { useCallback, useEffect, useRef, useState } from "react";

import { finishCapture, startCapture, type ActiveCapture, type SpeechGate } from "../lib/capture";
import { loadSileroSession, SpeechProbe } from "../lib/silero";
import { speechOnly } from "../lib/speech";
import { postCapture } from "./voice";

/**
 * `off` and not `idle`, matching `ConversationPhase` rather than `VoicePhase`.
 *
 * The two neighbours are the two controls that can sit in one composer, and a reader comparing them
 * should not have to hold two words for the same state. `VoicePhase` keeps `idle` because the Voice
 * page's reducer is a tested contract with the host process.
 */
export type DictationPhase = "off" | "listening" | "transcribing";

export interface DictationView {
  phase: DictationPhase;
  /** Why it stopped being able to do its job. Cleared by the next attempt, never by a timer. */
  trouble: string | null;
  toggle: () => void;
}

/**
 * Keeps only the part of a recording somebody was talking in, from a runtime loaded in parallel.
 *
 * **Why the gate is here at all.** Whisper does not answer "nothing" to audio with no speech in it; it
 * invents. Measured on this machine on 2026-09-18, a dictation of 40.5 s of a quiet room came back as
 * `[IMHA METALL [ ice / ice / ice …`, and another of 40.7 s as `Allah, iosha' mumu.` — text that would
 * have been pasted into whatever box was waiting for it. Whisper's own `--suppress-nst` measured worse
 * rather than better, so this is the layer that can fix it (see `lib/speech.ts` for the numbers).
 *
 * **Why it degrades to sending everything.** A runtime that will not load, or one that fails halfway,
 * has no opinion about this recording — and a microphone that silently discards a sentence is a worse
 * failure than a transcript nobody asked for. `lib/silero.ts` now says WHY it could not load, and that
 * reason is what `trouble` carries.
 */
function speechGate(loading: Promise<SpeechProbe | null>): SpeechGate {
  return async (samples) => {
    const probe = await loading;
    if (probe === null) return samples;
    try {
      return await speechOnly(samples, probe);
    } catch {
      return samples;
    }
  };
}

/**
 * A microphone that fills a text box.
 *
 * `onText` is held in a ref rather than captured. The callback is rebuilt on every render — it
 * closes over the draft it appends to — while the recording outlives many of them, so a captured one
 * would append to whatever the draft was when the microphone opened and lose everything typed since.
 */
export function useDictation(onText: (text: string) => void): DictationView {
  const [phase, setPhase] = useState<DictationPhase>("off");
  const [trouble, setTrouble] = useState<string | null>(null);
  const activeRef = useRef<ActiveCapture | null>(null);
  const textRef = useRef(onText);
  textRef.current = onText;

  /* The phase again, as a ref. `toggle` is built once and never rebuilt — it is handed to a button
     that must not change identity between renders — so it cannot read the state above. */
  const phaseRef = useRef<DictationPhase>("off");
  /* Between the press and an open microphone there is an await, and on the first ever press that
     await is a permission prompt somebody may leave standing. Without this, a second press during
     it takes the "nothing is recording" branch and opens a SECOND device. */
  const openingRef = useRef(false);
  /**
   * The speech detector, loading, from the moment the microphone opened.
   *
   * A promise and not a value, and started at the START of the recording: the runtime is 13 MB of
   * WebAssembly and the model 2.3 MB, so loading it when the recording STOPS would put all of that
   * between somebody finishing a sentence and the transcription beginning. Loaded while they talk, it
   * costs nothing — a person speaking is already the slow part. Kept across toggles, because the
   * second dictation should not pay for it again.
   */
  const probeRef = useRef<Promise<SpeechProbe | null> | null>(null);

  const move = useCallback((next: DictationPhase) => {
    phaseRef.current = next;
    setPhase(next);
  }, []);

  /* An open microphone must not survive the box it belongs to. Leaving the graph up on unmount
     leaves the device light on with nothing listening — and on the front door, unmounting is what
     happens the moment the first message opens a conversation. */
  useEffect(
    () => () => {
      const active = activeRef.current;
      activeRef.current = null;
      openingRef.current = false;
      if (active !== null) void finishCapture(active);
    },
    [],
  );

  const toggle = useCallback(() => {
    // The sentence just said is still being written down. The button is disabled through this, so
    // reaching here means a keyboard or a second window got in — and starting a new recording would
    // race two transcripts into the same box.
    if (openingRef.current || phaseRef.current === "transcribing") return;

    const active = activeRef.current;
    if (active === null) {
      openingRef.current = true;
      setTrouble(null);
      void (async () => {
        try {
          const opened = await startCapture();
          // Unmounted while the prompt was up. The graph exists now and nothing else will ever
          // close it, so close it here rather than leave the device open.
          if (!openingRef.current) {
            void finishCapture(opened);
            return;
          }
          activeRef.current = opened;
          probeRef.current ??= loadSileroSession().then(({ session, why }) => {
            // Reported rather than swallowed, and this is the only place it can be: a dictation with
            // no gate transcribes silence, which is the failure that produced `ice / ice / ice`.
            if (session === null) setTrouble("no speech gate — " + why);
            return session === null ? null : new SpeechProbe(session);
          });
          move("listening");
        } catch {
          setTrouble("no microphone — this machine refused or has none");
        } finally {
          openingRef.current = false;
        }
      })();
      return;
    }

    activeRef.current = null;
    move("transcribing");
    void (async () => {
      try {
        const { bytes, ms } = await finishCapture(
          active,
          speechGate(probeRef.current ?? Promise.resolve(null)),
        );
        // Nothing in the recording was speech, so there is nothing to transcribe — and the sentence
        // for it is the one the daemon's own 204 gets, because they are the same fact arriving from
        // different distances.
        if (bytes === null) {
          setTrouble("nothing was heard");
          return;
        }
        const result = await postCapture(bytes, "dictation", ms);
        if (result === undefined) {
          // 204, the one success-family status in this shell that is a negative answer — see
          // `data/voice.ts`'s header. Not red: saying nothing into an open microphone is ordinary.
          setTrouble("nothing was heard");
        } else {
          textRef.current(result.text);
        }
      } catch (error) {
        setTrouble(error instanceof Error ? error.message : "the dictation could not be sent");
      } finally {
        move("off");
      }
    })();
  }, [move]);

  return { phase, trouble, toggle };
}
