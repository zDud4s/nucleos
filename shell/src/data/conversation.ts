/**
 * Running a hands-free conversation: the microphone, the gate, the turns and the answer's audio.
 *
 * The decisions are elsewhere and on purpose. `lib/conversation.ts` owns what each event means,
 * `lib/vad.ts` owns when somebody started and stopped talking, and `core/src/voice.rs` owns what a
 * turn IS. What is left here is the part that cannot be pure: a `MediaStream`, an `AudioContext`, an
 * `<audio>` element, and the order they go in.
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
import { durationMs, encodeCapture } from "../lib/audio";
import {
  energyOf,
  FRAME_SAMPLES,
  GateState,
  IDLE_GATE,
  onFrame,
  PREROLL_FRAMES,
} from "../lib/vad";
import { loadSileroSession, SpeechProbe } from "../lib/silero";
import { fetchSpeechUnit, postConversation } from "./voice";

/** The chord's event, emitted by `shell/src-tauri/src/dictation.rs`. */
const TOGGLE_EVENT = "voice://conversation-toggle";

/**
 * Samples per `onaudioprocess` callback.
 *
 * A multiple of `FRAME_SAMPLES` so a callback splits into whole frames with nothing left over. A
 * remainder would have to be carried between callbacks, and a carry that is ever dropped shifts every
 * later frame — which reads as the gate becoming erratic rather than as an arithmetic mistake.
 */
const BUFFER_SAMPLES = FRAME_SAMPLES * 8;

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
  toggle: () => void;
}

interface Live {
  stream: MediaStream;
  context: AudioContext;
  source: MediaStreamAudioSourceNode;
  processor: ScriptProcessorNode;
  sink: GainNode;
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

  const phaseRef = useRef<ConversationPhase>("off");
  const chatRef = useRef<string | null>(chatId);
  const liveRef = useRef<Live | null>(null);
  const gateRef = useRef<GateState>(IDLE_GATE);
  /** The last `PREROLL_FRAMES` frames, kept always — see `PREROLL_FRAMES` for why. */
  const prerollRef = useRef<Float32Array[]>([]);
  const recordingRef = useRef<Float32Array[] | null>(null);
  const playerRef = useRef<HTMLAudioElement | null>(null);
  /** Bumped on every barge-in and every exit, so audio from an abandoned turn cannot start playing. */
  const generationRef = useRef(0);
  /** `null` until the model has been loaded, and permanently so if it could not be. */
  const probeRef = useRef<SpeechProbe | null>(null);
  /**
   * Inference is async and Silero's state is sequential, so frames are run one at a time in order.
   * Two in flight would interleave their state updates and the model's memory would describe audio
   * that never happened.
   */
  const chainRef = useRef<Promise<void>>(Promise.resolve());
  /** How many frames are waiting on the chain, so a slow runtime cannot grow it without bound. */
  const pendingRef = useRef(0);
  /**
   * Mirrors `listeningWith`, so the load is attempted once and not once per toggle.
   *
   * A ref beside the state because the check happens inside `openMic`, which is a stable callback:
   * reading the state there would read whatever it was when the callback was built, which is `null`
   * forever.
   */
  const listeningWithRef = useRef<"silero" | "energy" | null>(null);

  chatRef.current = chatId;

  const closeMic = useCallback(() => {
    const live = liveRef.current;
    liveRef.current = null;
    recordingRef.current = null;
    prerollRef.current = [];
    gateRef.current = IDLE_GATE;
    // Silero's state is a memory of what it just heard. Carried across a closed microphone, the next
    // conversation would start mid-thought about a sentence from the last one.
    probeRef.current?.reset();
    pendingRef.current = 0;
    if (live === null) return;
    live.processor.disconnect();
    live.source.disconnect();
    live.sink.disconnect();
    for (const track of live.stream.getTracks()) track.stop();
    void live.context.close();
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

  const sendTurn = useCallback(async () => {
    const frames = recordingRef.current ?? [];
    recordingRef.current = null;
    const chat = chatRef.current;
    const generation = generationRef.current;

    if (chat === null || chat === "") {
      setTrouble("open a chat first — a spoken turn has to belong to a conversation");
      dispatchRef.current({ type: "turnRefused" });
      return;
    }

    const samples = concat(frames);
    const rate = liveRef.current?.context.sampleRate ?? 16000;
    try {
      const result = await postConversation(
        encodeCapture(samples, 1, rate),
        chat,
        durationMs(samples.length, rate),
      );
      if (result === undefined) {
        // 204: the gate opened on something that was not speech after all. Common with the
        // placeholder energy source, and not worth a red message.
        dispatchRef.current({ type: "turnRefused" });
        return;
      }
      setHeard(result.text);
      if (result.turn_id === null) {
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
    try {
      // Loaded once and kept: the model is 2.3 MB and the runtime's wasm is larger still, so paying
      // for it on every toggle would put a second of dead air at the front of every session.
      if (probeRef.current === null && listeningWithRef.current === null) {
        const session = await loadSileroSession();
        probeRef.current = session === null ? null : new SpeechProbe(session);
        listeningWithRef.current = session === null ? "energy" : "silero";
        setListeningWith(listeningWithRef.current);
      }
      const stream = await navigator.mediaDevices.getUserMedia({
        // The three that make hands-free possible at all. `echoCancellation` is the load-bearing
        // one: the microphone is open WHILE the answer plays, so without it the gate hears the
        // núcleo's own voice, opens a turn on it, and the conversation talks to itself. It works
        // because capture and playback share this webview's audio graph — which is why
        // `lib/conversation.ts` keeps the playback here rather than in the other process.
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
      const AudioContextCtor =
        window.AudioContext ??
        (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
      const context = new AudioContextCtor();
      const source = context.createMediaStreamSource(stream);
      const processor = context.createScriptProcessor(BUFFER_SAMPLES, 1, 1);
      // Zero gain, for the reason `pages/Voice.tsx` records: the graph has to reach `destination`
      // for `onaudioprocess` to fire, and a live path there plays the microphone out loud.
      const sink = context.createGain();
      sink.gain.value = 0;

      processor.onaudioprocess = (event) => {
        const buffer = event.inputBuffer.getChannelData(0);
        for (let at = 0; at + FRAME_SAMPLES <= buffer.length; at += FRAME_SAMPLES) {
          const frame = new Float32Array(buffer.subarray(at, at + FRAME_SAMPLES));
          onAudioFrame(frame);
        }
      };
      source.connect(processor);
      processor.connect(sink);
      sink.connect(context.destination);
      liveRef.current = { stream, context, source, processor, sink };
      setTrouble(null);
    } catch {
      setTrouble("the microphone could not be opened");
      // Straight back out rather than sitting in `listening` with no microphone: a mode that looks
      // armed and hears nothing is the failure this pillar's design keeps calling out by name.
      dispatchRef.current({ type: "toggled" });
    }
  }, []);

  /**
   * One frame, through the gate, into whatever the mode makes of it.
   *
   * Kept in a ref and not a dependency, because `onaudioprocess` is assigned once when the graph is
   * built and would otherwise hold the first render's closure for the life of the microphone.
   */
  const onAudioFrame = useCallback((frame: Float32Array) => {
    // Synchronous and first, because these two are what the recording IS. Deferring them behind the
    // probe below would put the audio's order at the mercy of how fast inference happens to be.
    const recording = recordingRef.current;
    if (recording !== null) recording.push(frame);

    prerollRef.current.push(frame);
    if (prerollRef.current.length > PREROLL_FRAMES) prerollRef.current.shift();

    const decide = (probability: number) => {
      const { state, signal } = onFrame(gateRef.current, probability);
      gateRef.current = state;
      if (signal !== null) dispatchRef.current({ type: signal });
    };

    const probe = probeRef.current;
    if (probe === null) {
      decide(energyOf(frame));
      return;
    }

    // Dropped rather than queued once the chain is behind. A frame decided three frames late is a
    // barge-in noticed a tenth of a second late, and every one kept makes the next one later still —
    // the lag would grow for as long as the runtime stayed slow, and never recover.
    if (pendingRef.current > 3) return;

    pendingRef.current += 1;
    chainRef.current = chainRef.current
      .then(() => probe.probability(frame))
      .then(decide)
      // One frame that failed is one frame of silence, not a broken mode. A runtime that fails every
      // frame presents as a gate that never opens, which is what the fallback below is for.
      .catch(() => undefined)
      .finally(() => {
        pendingRef.current -= 1;
      });
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
          // Seeded with the pre-roll, so the recording contains the syllable the gate needed in
          // order to decide there was one.
          recordingRef.current = [...prerollRef.current];
          return;
        case "stopPlaybackAndRecord":
          stopPlayback();
          recordingRef.current = [...prerollRef.current];
          return;
        case "sendTurn":
          void sendTurn();
          return;
        case "nothing":
          return;
      }
    },
    [closeMic, openMic, sendTurn, stopPlayback],
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

  return { phase, heard, trouble, hasVoice, listeningWith, toggle };
}

function concat(frames: Float32Array[]): Float32Array {
  let length = 0;
  for (const frame of frames) length += frame.length;
  const out = new Float32Array(length);
  let at = 0;
  for (const frame of frames) {
    out.set(frame, at);
    at += frame.length;
  }
  return out;
}

function sentenceFor(error: unknown): string {
  return error instanceof Error ? error.message : "something went wrong";
}
