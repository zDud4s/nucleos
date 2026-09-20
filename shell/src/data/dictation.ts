/**
 * Talking into a box that has no conversation behind it yet, and seeing the words as you say them.
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
 * ## Progressive, since 2026-09-20
 *
 * It used to be one recording and one round trip: press, talk, press, wait, receive the lot. Now the
 * microphone stays open and the gate in `data/listening.ts` cuts it into sentences, and while a
 * sentence is still being spoken this asks the transcriber what it makes of the part said so far.
 * Each answer REPLACES the last — `lib/provisional.ts` owns that span of the box — because a
 * transcriber revises as it hears more context, and "come" becoming "come view" becoming "câmbio" is
 * one word being corrected, not three words being dictated.
 *
 * Two things make it work and neither is optional:
 *
 * - **A resident transcriber.** Measured on this machine on 2026-09-20, whisper.cpp's server answers
 *   a short clip in 96 ms where spawning `whisper-cli` takes 1250 ms, almost all of it process start
 *   and model load. A revision every 1.25 s is not progressive dictation. See `VoiceConfig::stt_url`.
 * - **Only the sentence in progress is re-sent**, never the whole dictation. A sentence is 3-8 s, so
 *   the cadence does not degrade the longer somebody talks — which is exactly what would happen if
 *   the growing recording were re-transcribed from the top.
 *
 * The call that ENDS a sentence goes as `kind=dictation`, and that is the one that writes the row in
 * `voice_captures` and is kept for `retain_dictations_days`. The drafts before it are `kind=draft`,
 * which writes nothing. Same audio for the last draft and the final, so the text does not change
 * under somebody's feet after they have stopped watching it.
 *
 * **No speech gate here any more, and that is not an oversight.** `lib/speech.ts` exists because
 * whisper invents words for silence, and a press-to-dictate could hand it forty seconds of a quiet
 * room. Nothing reaches the transcriber here that the gate in `lib/vad.ts` did not already declare
 * speech, frame by frame, as it arrived — so trimming the silence out of a span that is speech by
 * construction would be the same work done twice.
 */

import { useCallback, useRef, useState } from "react";

import { encodeCapture } from "../lib/audio";
import { useListening, type Listening } from "./listening";
import { postCapture, postDraft } from "./voice";

/**
 * Frames of speech between one draft and the next being ASKED for — 8 frames, a little over a
 * quarter of a second.
 *
 * A floor and not a cadence: only one draft is ever in flight, so what actually paces the revisions
 * is how fast the transcriber answers. This exists for the machine where it answers fast, so that a
 * 96 ms round trip does not become thirty requests a second for a sentence nobody has finished.
 */
export const DRAFT_EVERY_FRAMES = 8;

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
 * A microphone that fills a text box, revising as it goes.
 *
 * `onText` is handed the whole of what the microphone currently believes the sentence in progress
 * to be, and `final` says whether it will ever change again. A caller replaces the same span of its
 * box on every call until `final`, and then leaves it alone.
 *
 * It is held in a ref rather than captured. The callback is rebuilt on every render — it closes over
 * the draft it writes into — while the microphone outlives many of them, so a captured one would
 * write into whatever the draft was when the microphone opened and lose everything typed since.
 */
export function useDictation(onText: (text: string, final: boolean) => void): DictationView {
  const [phase, setPhase] = useState<DictationPhase>("off");
  const [trouble, setTrouble] = useState<string | null>(null);
  const textRef = useRef(onText);
  textRef.current = onText;

  /* The phase again, as a ref. `toggle` is built once and never rebuilt — it is handed to a button
     that must not change identity between renders — so it cannot read the state above. */
  const phaseRef = useRef<DictationPhase>("off");
  /* Between the press and an open microphone there is an await, and on the first ever press that
     await is a permission prompt somebody may leave standing. Without this, a second press during
     it takes the "nothing is listening" branch and opens a SECOND device. */
  const openingRef = useRef(false);

  /** Which sentence is being spoken. Every draft carries the number of the sentence it guessed at. */
  const sentenceRef = useRef(0);
  /** The highest sentence already written down. A draft at or below it has been overtaken. */
  const writtenRef = useRef(0);
  /** Frames of speech since the last draft was asked for. */
  const sinceDraftRef = useRef(0);
  /** One draft at a time — see `DRAFT_EVERY_FRAMES`. */
  const draftingRef = useRef(false);
  /**
   * The finals, in speech order, and what every draft waits on before it writes.
   *
   * Two orderings have to hold and they are not the same one. Finals must land in the order the
   * sentences were spoken, which is what chaining them does. And a draft must never land after the
   * final of its own sentence — a guess re-opening a span over text somebody has stopped expecting
   * to move is the worst thing this loop can do — which is what waiting on the chain, and then
   * re-reading `writtenRef`, does.
   */
  const finalsRef = useRef<Promise<void>>(Promise.resolve());

  const move = useCallback((next: DictationPhase) => {
    phaseRef.current = next;
    setPhase(next);
  }, []);

  /* The microphone, reached through a ref because the handlers below are part of how it is built and
     would otherwise have to close over it before it exists. */
  const micRef = useRef<Listening | null>(null);

  /**
   * The sentence that just ended, sent as the dictation it is. `false` when there was none.
   *
   * Enqueued rather than awaited, so the microphone is free to start the next sentence immediately.
   * Until 2026-09-20 `toggle` refused to record while a transcription was in flight, which was right
   * when a dictation WAS one recording — here it would make the microphone deaf for a round trip at
   * exactly the moment somebody draws breath and carries on.
   */
  const writeDown = useCallback((): boolean => {
    const mic = micRef.current;
    if (mic === null || !mic.isRecording()) return false;
    const recorded = mic.take();
    if (recorded === null) return false;
    const sentence = sentenceRef.current;

    finalsRef.current = finalsRef.current.then(async () => {
      try {
        const bytes = encodeCapture(recorded.samples, 1, recorded.rate);
        const result = await postCapture(bytes, "dictation", recorded.ms);
        writtenRef.current = Math.max(writtenRef.current, sentence);
        if (result === undefined) {
          // 204, the one success-family status in this shell that is a negative answer — see
          // `data/voice.ts`'s header. Not red: saying nothing into an open microphone is ordinary.
          // The empty final is what takes the drafts back out of the box.
          setTrouble("nothing was heard");
        }
        textRef.current(result?.text ?? "", true);
      } catch (error) {
        writtenRef.current = Math.max(writtenRef.current, sentence);
        setTrouble(error instanceof Error ? error.message : "the dictation could not be sent");
      }
    });
    return true;
  }, []);

  const askForDraft = useCallback(() => {
    const mic = micRef.current;
    if (mic === null || draftingRef.current) return;
    const recorded = mic.peek();
    if (recorded === null) return;
    const sentence = sentenceRef.current;

    draftingRef.current = true;
    void (async () => {
      try {
        const bytes = encodeCapture(recorded.samples, 1, recorded.rate);
        const said = await postDraft(bytes, recorded.ms);
        await finalsRef.current;
        if (sentence <= writtenRef.current) return;
        textRef.current(said, false);
      } catch {
        // A draft that failed is a revision that does not happen. Saying so would put a sentence
        // about the network in front of somebody mid-word, for something the next draft fixes.
      } finally {
        draftingRef.current = false;
      }
    })();
  }, []);

  const mic = useListening({
    onFrame: (recording) => {
      if (!recording) {
        sinceDraftRef.current = 0;
        return;
      }
      sinceDraftRef.current += 1;
      if (sinceDraftRef.current < DRAFT_EVERY_FRAMES) return;
      sinceDraftRef.current = 0;
      /* Off the audio callback, which is this thread's most time-critical caller: joining the
         sentence so far and encoding it to WAV is a few milliseconds of arithmetic over a growing
         buffer, and spending them here is spending them between two frames of somebody's voice.
         Nothing waits on it — the next frame is what would, and it is 32 ms away. */
      setTimeout(askForDraft, 0);
    },
    onSignal: (signal) => {
      if (signal === "speechStarted") {
        sentenceRef.current += 1;
        sinceDraftRef.current = 0;
        micRef.current?.record();
        return;
      }
      writeDown();
    },
    onTrouble: setTrouble,
    onDetector: (using, why) => {
      // Reported rather than swallowed, and this is the only place it can be: a dictation with no
      // gate transcribes silence, which is the failure that produced `ice / ice / ice`.
      if (using === "energy") setTrouble("no speech gate — " + why);
    },
  });
  micRef.current = mic;

  const toggle = useCallback(() => {
    // The last sentence is still being written down and the microphone is already shut. The button
    // is disabled through this, so reaching here means a keyboard or a second window got in.
    if (openingRef.current || phaseRef.current === "transcribing") return;

    if (phaseRef.current === "off") {
      openingRef.current = true;
      setTrouble(null);
      sentenceRef.current = 0;
      writtenRef.current = 0;
      void (async () => {
        try {
          if (await (micRef.current?.open(true) ?? Promise.resolve(false))) move("listening");
        } finally {
          openingRef.current = false;
        }
      })();
      return;
    }

    // Taken BEFORE the microphone is closed — closing drops the recording, and pressing stop in the
    // middle of a sentence is how most dictations end. Nobody waits out the hangover.
    const writing = writeDown();
    micRef.current?.close();
    if (!writing) {
      move("off");
      return;
    }
    move("transcribing");
    void finalsRef.current.finally(() => move("off"));
  }, [move, writeDown]);

  return { phase, trouble, toggle };
}
