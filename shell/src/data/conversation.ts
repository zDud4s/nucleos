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
import {
  diagnoseListening,
  FRAME_MS,
  gateFor,
  selfGuardHolds,
  type SilenceReason,
} from "../lib/vad";
import { isEcho } from "../lib/echo";
import type { AssistantTurnRow } from "../lib/turns";
import { useListening, type Listening } from "./listening";
import type { LiveTurn } from "./chats";
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

/** Four seconds is long enough to distinguish a broken input from a pause before speaking. */
export const SILENCE_AFTER_FRAMES = Math.round(4000 / FRAME_MS);
/** Peaks over this recent window describe the signal without mistaking a gap between syllables for a fault. */
export const SILENCE_WINDOW_FRAMES = Math.round(1500 / FRAME_MS);
/** An answer can linger in the microphone briefly after its final audio unit. */
export const ECHO_TAIL_MS = 1000;
/** A meter this size cannot show smaller movement clearly enough to justify another render. */
export const LEVEL_STEP = 0.05;

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
  /** The microphone's latest rounded level, whether energy or Silero is judging speech. */
  level: number;
  /** The gate currently in force, so a meter can be drawn against the bar it is actually judged by. */
  threshold: number;
  /** Segments already understood while the current turn is still being assembled. */
  assembling: string | null;
  /** Why a live listening microphone has not opened a turn yet. */
  silence: SilenceReason | null;
  /** Answer fragments discarded before they could become a false new turn. */
  ignoredEcho: number;
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
  const [level, setLevel] = useState(0);
  const [assembling, setAssembling] = useState<string | null>(null);
  const [silence, setSilence] = useState<SilenceReason | null>(null);
  const [ignoredEcho, setIgnoredEcho] = useState(0);

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

  /* What the window is shown, mirrored in refs: all of it is decided inside the audio callback, which
     runs 31 times a second and must not read state it would be a render behind on. */
  const levelRef = useRef(0);
  const assemblingRef = useRef<string | null>(null);
  const silenceRef = useRef<SilenceReason | null>(null);
  const listeningFramesRef = useRef(0);
  const loudnessWindowRef = useRef<number[]>([]);
  const probabilityWindowRef = useRef<number[]>([]);
  /** When the frame now being judged was captured — see `onMeasured`, and `SELF_GUARD_MS` for why. */
  const lastFrameAtRef = useRef(Number.NEGATIVE_INFINITY);
  /** Each unit needs its own guard: its first syllable arrives before cancellation adapts. */
  const unitStartedAtRef = useRef(Number.NEGATIVE_INFINITY);
  /** Words are fetched separately because speech units carry audio only, never their text. */
  const spokenRef = useRef("");
  /** The saved answer of one turn, which cannot change again — see `refreshSpoken`. */
  const savedAnswerRef = useRef<{ turnId: number; text: string } | null>(null);
  const answerEndedAtRef = useRef(Number.NEGATIVE_INFINITY);
  const recordingNearAnswerRef = useRef(false);
  /**
   * Which detector is judging, as a ref.
   *
   * `diagnoseListening` needs it inside the audio callback, and the state above is what the window
   * reads. Reading state there would read whatever it was when the callback was built, which is
   * `null` for the life of the microphone.
   */
  const listeningWithRef = useRef<"silero" | "energy" | null>(null);

  const publishAssembling = useCallback(() => {
    const next = turnRef.current.segments.join(" ") || null;
    if (assemblingRef.current === next) return;
    assemblingRef.current = next;
    setAssembling(next);
  }, []);

  const publishSilence = useCallback((next: SilenceReason | null) => {
    if (silenceRef.current === next) return;
    silenceRef.current = next;
    setSilence(next);
  }, []);

  const publishLevel = useCallback((next: number) => {
    // A movement nobody can see on the meter is not worth a render 31 times a second. Zero is always
    // published, though: it is the difference between a quiet room and a microphone that stopped.
    if (Math.abs(next - levelRef.current) < LEVEL_STEP && !(next === 0 && levelRef.current !== 0)) {
      return;
    }
    levelRef.current = next;
    setLevel(next);
  }, []);

  const resetListeningDiagnostics = useCallback(() => {
    listeningFramesRef.current = 0;
    loudnessWindowRef.current = [];
    probabilityWindowRef.current = [];
    publishSilence(null);
  }, [publishSilence]);

  /**
   * Why a listening microphone has not opened a turn, once it has listened long enough for the
   * question to be a fair one.
   *
   * `micOpen` is ASKED rather than assumed, which is the whole of what `isOpen` buys here — and being
   * honest about it makes plain that `noMicrophone` is still unreachable from this path: no frame
   * arrives through a closed device, so nothing calls this. A device that ends under a live session is
   * trouble rather than a kind of quiet, and wiring the track's own `ended` event is a separate fix.
   */
  const updateListeningDiagnosis = useCallback(() => {
    if (phaseRef.current !== "listening" || listeningFramesRef.current < SILENCE_AFTER_FRAMES) {
      publishSilence(null);
      return;
    }
    publishSilence(
      diagnoseListening({
        micOpen: listenerRef.current?.isOpen() ?? false,
        loudness: Math.max(0, ...loudnessWindowRef.current),
        probability: Math.max(0, ...probabilityWindowRef.current),
        // This branch has already established that the runner is listening, so this is its resting bar.
        threshold: gateFor(false).enter,
        detector: listeningWithRef.current,
      }),
    );
  }, [publishSilence]);

  chatRef.current = chatId;

  /* The turn, and nothing about the audio — `listening.ts` owns the graph, the gate and Silero's
     memory of what it just heard, and resets all three on its way out. */
  const closeMic = useCallback(() => {
    listenerRef.current?.close();
    turnRef.current = EMPTY_TURN;
    publishAssembling();
    resetListeningDiagnostics();
    pendingTurnRef.current = null;
    segmentChainRef.current = Promise.resolve();
    // A segment already at the core cannot be cancelled, so invalidate its answer before closing.
    generationRef.current += 1;
  }, [publishAssembling, resetListeningDiagnostics]);

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
  const refreshSpoken = useCallback((turnId: number) => {
    void (async () => {
      try {
        const live = await apiFetch<LiveTurn | undefined>(`/assistant/${turnId}/live`);
        if (live?.text !== undefined) {
          spokenRef.current = live.text;
          return;
        }
      } catch {
        // `/live` is an in-memory tail, so a completed turn may have already dropped it.
      }

      // Read once per turn and not once per unit. A saved answer cannot change again, and the read is
      // the whole chat — up to a hundred turns — which a long answer would otherwise repeat per
      // sentence, against the daemon, while it is speaking.
      const saved = savedAnswerRef.current;
      if (saved !== null && saved.turnId === turnId) {
        spokenRef.current = saved.text;
        return;
      }

      const chat = chatRef.current;
      if (chat === null || chat === "") return;
      try {
        const answered = await apiFetch<{ turns: AssistantTurnRow[] }>(
          `/assistant/chats/${encodeURIComponent(chat)}`,
        );
        const answer = answered.turns.find((turn) => turn.id === turnId)?.answer;
        if (typeof answer === "string") {
          savedAnswerRef.current = { turnId, text: answer };
          spokenRef.current = answer;
        }
      } catch {
        // Keep the last spoken words when neither source is available.
      }
    })();
  }, []);

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
          // `/live` reads the run's in-memory tail, which `Registration::drop` removes when a turn
          // finishes. A short answer usually finishes before its first unit plays, so the saved turn
          // is the source that outlives it for the echo judge.
          refreshSpoken(turnId);
          await play(unit.wav, generation);
          if (generationRef.current !== generation) return;
        }
      } catch (error) {
        setTrouble(sentenceFor(error));
        emit({ type: "answerEnded" });
      }
    },
    [refreshSpoken],
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
        // Per unit, not per answer: the attack of each unit's first syllable is its loudest, least
        // cancelled moment, and an answer is many units long.
        unitStartedAtRef.current = performance.now();
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
    // The scope belongs to THIS recording, read now: the next recording can start while this one is
    // still being transcribed, and a scope read later would be the next sentence's.
    const nearAnswer = recordingNearAnswerRef.current;
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
        // A late echo becomes the same empty continuing segment as a 204: no new transition is needed,
        // and the turn stays open for the person's actual words.
        const echo = nearAnswer && isEcho(segment.text, spokenRef.current);
        if (echo) setIgnoredEcho((count) => count + 1);
        const accepted = echo ? { text: "", verdict: "continues" as const } : segment;
        const next = onSegment(turnRef.current, { ...accepted, elapsedMs });
        turnRef.current = next.state;
        publishAssembling();
        actOnTurnSignal(next.signal);
      } catch (error) {
        if (generationRef.current === generation) setTrouble(sentenceFor(error));
      }
    });
  }, [actOnTurnSignal, publishAssembling]);

  const sendTurn = useCallback(async () => {
    const text = pendingTurnRef.current ?? "";
    pendingTurnRef.current = null;
    // The previous answer's words stop being the yardstick the moment a new turn goes out: judging
    // this answer's echo against the last one's vocabulary is how a real question gets eaten.
    spokenRef.current = "";
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
          // A question that reuses the answer's words minutes later is still a question, so the scope
          // is deliberately the answer and its short acoustic tail, nothing more.
          recordingNearAnswerRef.current =
            performance.now() - answerEndedAtRef.current < ECHO_TAIL_MS;
          listenerRef.current?.record();
          return;
        case "stopPlaybackAndRecord":
          stopPlayback();
          // Only ever produced from `speaking`, so this is always a barge-in — and it still stops the
          // answer at the first open gate, before any transcript exists. The judge below can stop an
          // echo from becoming a turn; it cannot un-stop the audio. Ducking or a deferred stop needs a
          // new transition in the state machine and is not this phase's work.
          recordingNearAnswerRef.current = true;
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
      const was = phaseRef.current;
      const next = onConversationEvent(was, event);
      if (event.type === "answerEnded") answerEndedAtRef.current = performance.now();
      if (event.type === "toggled" && next.phase === "off") setIgnoredEcho(0);
      phaseRef.current = next.phase;
      // Every phase change starts the diagnosis over: the four seconds of evidence it needs are about
      // the state it is in, and carrying a window across the edge would explain the wrong one.
      if (next.phase !== was) resetListeningDiagnostics();
      if (next.phase === "off") publishLevel(0);
      setPhase(next.phase);
      perform(next.action);
    },
    [perform, publishLevel, resetListeningDiagnostics],
  );

  // Held in a ref so the audio callback and the async turns above call the CURRENT dispatch rather
  // than the one that existed when the graph was built.
  const dispatchRef = useRef(dispatch);
  dispatchRef.current = dispatch;

  /* Declared after `dispatchRef` because every handler below goes through it, and placed in a ref so
     the callbacks above — built before this line runs — can reach the microphone they drive. */
  listenerRef.current = useListening({
    onFrame: (recording) => {
      // Counted here, for every frame, and NOT beside the measurements below: this is how long the
      // mode has been listening, which every frame advances, while the evidence below belongs only to
      // frames that were actually judged. Counting only judged frames makes the four seconds arrive
      // late — or never, on a runtime slow enough to be dropping them, which is the very machine the
      // explanation exists for.
      if (phaseRef.current === "listening") listeningFramesRef.current += 1;
      if (recording) return;
      // Until transcription returns, `onSegment` cannot zero the clock. Counting recorded frames
      // here can therefore abandon the mode in the middle of a sentence that began near the limit.
      const idle = onIdle(turnRef.current, FRAME_MS);
      turnRef.current = idle.state;
      publishAssembling();
      if (idle.signal === null) return;
      actOnTurnSignal(idle.signal);
      // The frame goes no further: the mode is on its way out, and letting the gate see it would
      // open a segment on the way.
      return false;
    },
    /* The bar rises while the answer plays and drops the moment it stops, which is why this is asked
       per frame rather than chosen when the microphone opened. */
    gateNow: () => gateFor(phaseRef.current === "speaking"),
    onMeasured: ({ probability, loudness, at }) => {
      lastFrameAtRef.current = at;
      // The meter shows the value the GATE judged, not loudness: loudness against a probability bar
      // would show noise crossing the mark without a turn ever opening.
      publishLevel(Number(probability.toFixed(2)));
      if (phaseRef.current === "listening") {
        loudnessWindowRef.current.push(loudness);
        if (loudnessWindowRef.current.length > SILENCE_WINDOW_FRAMES) loudnessWindowRef.current.shift();
        probabilityWindowRef.current.push(probability);
        if (probabilityWindowRef.current.length > SILENCE_WINDOW_FRAMES) {
          probabilityWindowRef.current.shift();
        }
      }
      updateListeningDiagnosis();
    },
    onSignal: (signal) => {
      if (
        signal === "speechStarted" &&
        phaseRef.current === "speaking" &&
        selfGuardHolds(lastFrameAtRef.current - unitStartedAtRef.current)
      ) {
        // Refused, which re-arms the gate — see `onSignal` in `listening.ts`. Measured from when the
        // FRAME was captured, not from now: the probe answers a frame or two late, and a guard that
        // loses that much of its 350 ms lets the answer's own attack open a turn.
        updateListeningDiagnosis();
        return false;
      }
      dispatchRef.current({ type: signal });
    },
    onTrouble: setTrouble,
    onDetector: (using, why) => {
      listeningWithRef.current = using;
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

  return {
    phase,
    heard,
    trouble,
    hasVoice,
    listeningWith,
    whyByLoudness,
    level,
    threshold: gateFor(phase === "speaking").enter,
    assembling,
    silence,
    ignoredEcho,
    toggle,
  };
}

function sentenceFor(error: unknown): string {
  return error instanceof Error ? error.message : "something went wrong";
}
