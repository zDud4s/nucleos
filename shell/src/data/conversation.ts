/**
 * Running a hands-free conversation: the microphone, the gate, the turns and the answer's audio.
 *
 * The decisions are elsewhere and on purpose. `lib/conversation.ts` owns what each event means,
 * `lib/vad.ts` owns when somebody started and stopped talking, `data/listening.ts` owns the open
 * microphone that applies it, and `core/src/voice.rs` owns what a turn IS. What is left here is the
 * half that makes it a CONVERSATION rather than a transcript: the turn being assembled, the message
 * sent to the agent, and the answer played back.
 *
 * That last split is recent. Everything about hearing a sentence lived here until 2026-09-20, when
 * progressive dictation needed the same ears without the mouth — `data/dictation.ts` now drives the
 * same `useListening` and does something else entirely with what it hears. The seam between the two
 * halves turned out to be one edge: a closed turn becoming a sent one.
 *
 * The same division `pages/Voice.tsx` already lives under, one layer up.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  ConversationAction,
  ConversationEvent,
  ConversationPhase,
  onConversationEvent,
} from "../lib/conversation";
import { encodeCapture } from "../lib/audio";
import {
  EMPTY_TURN,
  onIdle,
  onSegment,
  TurnSignal,
  TurnState,
} from "../lib/turn-assembly";
import { FRAME_MS } from "../lib/vad";
import { useListening, type Listening } from "./listening";
import { apiFetch } from "./client";
import { fetchSpeechUnit, postSegment } from "./voice";

/** The chord's event, emitted by `shell/src-tauri/src/dictation.rs`. */
const TOGGLE_EVENT = "voice://conversation-toggle";

/**
 * The three constraints that make hands-free possible at all.
 *
 * `echoCancellation` is the load-bearing one: the microphone is open WHILE the answer plays, so
 * without it the gate hears the núcleo's own voice, opens a turn on it, and the conversation talks
 * to itself. It works because capture and playback share this webview's audio graph — which is why
 * this module keeps the playback here rather than in the other process.
 */
const CONVERSATION_AUDIO: MediaTrackConstraints = {
  echoCancellation: true,
  noiseSuppression: true,
  autoGainControl: true,
};

/** What the window needs to show, and nothing it does not. */
export interface ConversationView {
  phase: ConversationPhase;
  /** The last thing the transcriber heard, so a misheard question is visible before it is answered. */
  heard: string | null;
  /** Why the mode stopped being able to do its job, when it did. */
  trouble: string | null;
  /** Whether this machine can speak an answer at all. `false` means the answer is read, not heard. */
  hasVoice: boolean;
  /**
   * What is deciding whether somebody is talking.
   *
   * Surfaced rather than kept private because the two behave visibly differently: `"energy"` opens
   * turns on a fan or a fridge, and somebody watching that happen deserves to know it is the fallback
   * talking rather than the feature being broken. `null` until the microphone has been opened once.
   */
  listeningWith: "silero" | "energy" | null;
  /**
   * Why it is listening by loudness, in the runtime's own words, or `null` when it is not.
   *
   * Carried beside `listeningWith` because "it fell back" and "it fell back BECAUSE the policy refuses
   * to compile WebAssembly" cost very different amounts to act on, and for one session this app knew
   * the second and reported the first.
   */
  whyByLoudness: string | null;
  toggle: () => void;
}

/**
 * The hands-free conversation, for one chat.
 *
 * `chatId` is read through a ref rather than captured, because the audio graph outlives a re-render
 * and a captured id would keep sending turns to whichever chat was open when the microphone opened.
 */
export function useVoiceConversation(chatId: string | null): ConversationView {
  const [phase, setPhase] = useState<ConversationPhase>("off");
  const [heard, setHeard] = useState<string | null>(null);
  const [trouble, setTrouble] = useState<string | null>(null);
  const [hasVoice, setHasVoice] = useState(true);
  const [listeningWith, setListeningWith] = useState<"silero" | "energy" | null>(null);
  const [whyByLoudness, setWhyByLoudness] = useState<string | null>(null);

  const phaseRef = useRef<ConversationPhase>("off");
  const chatRef = useRef<string | null>(chatId);
  /** The ears: the microphone, the gate and the pre-roll, shared with `data/dictation.ts`. */
  const listenerRef = useRef<Listening | null>(null);
  const turnRef = useRef<TurnState>(EMPTY_TURN);
  const pendingTurnRef = useRef<string | null>(null);
  /** Segment requests stay in speech order even when the next pause arrives before one completes. */
  const segmentChainRef = useRef<Promise<void>>(Promise.resolve());
  const playerRef = useRef<HTMLAudioElement | null>(null);
  /** Bumped on every barge-in and every exit, so audio from an abandoned turn cannot start playing. */
  const generationRef = useRef(0);

  chatRef.current = chatId;

  /* The turn, and nothing about the audio — `listening.ts` owns the graph, the gate and Silero's
     memory of what it just heard, and resets all three on its way out. */
  const closeMic = useCallback(() => {
    listenerRef.current?.close();
    turnRef.current = EMPTY_TURN;
    pendingTurnRef.current = null;
    segmentChainRef.current = Promise.resolve();
    // A segment already at the core cannot be cancelled, so invalidate its answer before closing.
    generationRef.current += 1;
  }, []);

  const stopPlayback = useCallback(() => {
    // Bumped BEFORE the element is touched: a fetch already in flight checks this number before it
    // plays, so raising it here is what makes the interruption cover audio that has not arrived yet.
    generationRef.current += 1;
    const player = playerRef.current;
    if (player === null) return;
    player.pause();
    if (player.src !== "") URL.revokeObjectURL(player.src);
    player.removeAttribute("src");
  }, []);

  /**
   * Plays a turn's answer, one unit at a time, until it runs out or something interrupts.
   *
   * Fetches the next unit WHILE the current one plays. That costs one wasted synthesis when somebody
   * barges in — and buys a gapless answer, because waiting until a sentence ends to ask for the next
   * one puts a round trip of silence between every pair of sentences. `speak.rs`'s pull design is
   * what makes one unit the whole of the waste.
   */
  const speakAnswer = useCallback(
    async (turnId: number, generation: number) => {
      const emit = (event: ConversationEvent) => dispatchRef.current(event);
      let index = 0;
      let pending = fetchSpeechUnit(turnId, index);
      let started = false;

      try {
        for (;;) {
          const unit = await pending;
          if (generationRef.current !== generation) return;

          if (unit.type === "noVoice") {
            setHasVoice(false);
            emit({ type: "answerEnded" });
            return;
          }
          if (unit.type === "ended") {
            emit({ type: "answerEnded" });
            return;
          }
          if (unit.type === "notYet") {
            // The turn is still thinking. `speakable` withholds a half-written sentence, so this is
            // the ordinary answer for most of a turn's life rather than a sign of anything wrong.
            await new Promise((resume) => setTimeout(resume, 250));
            pending = fetchSpeechUnit(turnId, index);
            continue;
          }

          if (!started) {
            started = true;
            emit({ type: "answerStarted" });
          }
          // Asked for before this one plays, not after — the gap between sentences is the cost of
          // getting this order wrong.
          index += 1;
          pending = fetchSpeechUnit(turnId, index);
          await play(unit.wav, generation);
          if (generationRef.current !== generation) return;
        }
      } catch (error) {
        setTrouble(sentenceFor(error));
        emit({ type: "answerEnded" });
      }
    },
    [],
  );

  const play = useCallback(async (wav: Blob, generation: number) => {
    if (generationRef.current !== generation) return;
    const player = playerRef.current ?? new Audio();
    playerRef.current = player;
    const url = URL.createObjectURL(wav);
    player.src = url;
    try {
      await new Promise<void>((done, fail) => {
        player.onended = () => done();
        player.onerror = () => fail(new Error("the answer could not be played"));
        void player.play().catch(fail);
      });
    } finally {
      player.onended = null;
      player.onerror = null;
      URL.revokeObjectURL(url);
    }
  }, []);

  const actOnTurnSignal = useCallback((signal: TurnSignal | null) => {
    if (signal === null) return;
    switch (signal.type) {
      case "deliver":
        pendingTurnRef.current = signal.text;
        setHeard(signal.text);
        dispatchRef.current({ type: "turnClosed" });
        return;
      case "discard":
        dispatchRef.current({ type: "turnDiscarded" });
        return;
      case "confirmDiscard":
        setHeard("discard this turn?");
        return;
      case "abandon":
        dispatchRef.current({ type: "toggled" });
        return;
    }
  }, []);

  const transcribeSegment = useCallback(() => {
    /* Nothing recorded is still a segment. `onSegment` is what zeroes the turn's idle clock, and a
       pause that skipped it would abandon a turn somebody was in the middle of. */
    const recorded = listenerRef.current?.take() ?? { samples: new Float32Array(0), rate: 16000, ms: 0 };
    const elapsedMs = recorded.ms;
    const generation = generationRef.current;

    segmentChainRef.current = segmentChainRef.current.then(async () => {
      try {
        const segment = await postSegment(
          encodeCapture(recorded.samples, 1, recorded.rate),
          elapsedMs,
        );
        if (generationRef.current !== generation) return;
        const next = onSegment(turnRef.current, { ...segment, elapsedMs });
        turnRef.current = next.state;
        actOnTurnSignal(next.signal);
      } catch (error) {
        if (generationRef.current === generation) setTrouble(sentenceFor(error));
      }
    });
  }, [actOnTurnSignal]);

  const sendTurn = useCallback(async () => {
    const text = pendingTurnRef.current ?? "";
    pendingTurnRef.current = null;
    const chat = chatRef.current;
    const generation = generationRef.current;

    if (chat === null || chat === "") {
      setTrouble("open a chat first — a spoken turn has to belong to a conversation");
      dispatchRef.current({ type: "turnRefused" });
      return;
    }

    try {
      const result = await apiFetch<{ turn_id?: number; queued?: boolean }>(
        "/assistant/message",
        {
          method: "POST",
          body: JSON.stringify({
            chat_id: chat,
            text,
            wait_if_busy: true,
            origin: "voice",
          }),
        },
      );
      if (result.turn_id == null) {
        // Queued behind a turn already in flight. There is nothing to listen to yet, and the answer
        // that eventually comes belongs to the message that was already running.
        dispatchRef.current({ type: "turnRefused" });
        return;
      }
      void speakAnswer(result.turn_id, generation);
    } catch (error) {
      setTrouble(sentenceFor(error));
      dispatchRef.current({ type: "turnRefused" });
    }
  }, [speakAnswer]);

  const openMic = useCallback(async () => {
    if (await (listenerRef.current?.open(CONVERSATION_AUDIO) ?? Promise.resolve(false))) {
      setTrouble(null);
      return;
    }
    // Straight back out rather than sitting in `listening` with no microphone: a mode that looks
    // armed and hears nothing is the failure this pillar's design keeps calling out by name.
    dispatchRef.current({ type: "toggled" });
  }, []);

  const perform = useCallback(
    (action: ConversationAction) => {
      switch (action) {
        case "openMic":
          void openMic();
          return;
        case "closeMic":
          closeMic();
          return;
        case "stopPlaybackAndCloseMic":
          stopPlayback();
          closeMic();
          return;
        case "startRecording":
          listenerRef.current?.record();
          return;
        case "stopPlaybackAndRecord":
          stopPlayback();
          listenerRef.current?.record();
          return;
        case "transcribeSegment":
          transcribeSegment();
          return;
        case "sendTurn":
          void sendTurn();
          return;
        case "nothing":
          return;
      }
    },
    [closeMic, openMic, sendTurn, stopPlayback, transcribeSegment],
  );

  const dispatch = useCallback(
    (event: ConversationEvent) => {
      const next = onConversationEvent(phaseRef.current, event);
      phaseRef.current = next.phase;
      setPhase(next.phase);
      perform(next.action);
    },
    [perform],
  );

  // Held in a ref so the audio callback and the async turns above call the CURRENT dispatch rather
  // than the one that existed when the graph was built.
  const dispatchRef = useRef(dispatch);
  dispatchRef.current = dispatch;

  /* Declared after `dispatchRef` because every handler below goes through it, and placed in a ref so
     the callbacks above — built before this line runs — can reach the microphone they drive. */
  listenerRef.current = useListening({
    onFrame: (recording) => {
      if (recording) return;
      // Until transcription returns, `onSegment` cannot zero the clock. Counting recorded frames
      // here can therefore abandon the mode in the middle of a sentence that began near the limit.
      const idle = onIdle(turnRef.current, FRAME_MS);
      turnRef.current = idle.state;
      if (idle.signal === null) return;
      actOnTurnSignal(idle.signal);
      // The frame goes no further: the mode is on its way out, and letting the gate see it would
      // open a segment on the way.
      return false;
    },
    onSignal: (signal) => dispatchRef.current({ type: signal }),
    onTrouble: setTrouble,
    onDetector: (using, why) => {
      setListeningWith(using);
      setWhyByLoudness(why);
    },
  });

  useEffect(() => {
    let stop: (() => void) | undefined;
    listen(TOGGLE_EVENT, () => dispatchRef.current({ type: "toggled" }))
      .then((off) => {
        stop = off;
      })
      // Swallowed rather than surfaced, and the mode keeps working without it. Subscribing needs a
      // Tauri runtime; a test harness and a plain browser have none, and neither does a shell whose
      // global-shortcut plugin failed to start. What is lost is the chord — the button beside Send
      // is unaffected, and it is the button that a person who cannot see the chord will reach for.
      .catch(() => undefined);
    return () => stop?.();
  }, []);

  // The microphone must not outlive the page. Without this, navigating away leaves a live capture
  // with nothing on screen saying so — which in a pillar built for privacy is the worst leak it has.
  useEffect(() => closeMic, [closeMic]);

  const toggle = useCallback(() => dispatchRef.current({ type: "toggled" }), []);

  return { phase, heard, trouble, hasVoice, listeningWith, whyByLoudness, toggle };
}

function sentenceFor(error: unknown): string {
  return error instanceof Error ? error.message : "something went wrong";
}
