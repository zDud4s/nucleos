/**
 * Opening a microphone, keeping what it hears, and closing it again.
 *
 * The audio graph and nothing else: no route, no phase, no opinion about what the samples are for.
 * `data/voice.ts` owns what a capture is POSTED as, `lib/audio.ts` owns how it is encoded, and the
 * two callers own what a finished recording means.
 *
 * **One copy because the graph has a trap in it that is invisible from the outside.** A
 * `ScriptProcessorNode` only fires `onaudioprocess` once the graph reaches `destination`, and
 * wiring the microphone straight there plays it out loud through the speakers while it records —
 * so a zero-gain node sits between them. Neither half of that is guessable, neither is visible in
 * jsdom (which has no `AudioContext` and no `getUserMedia`), and a second copy that dropped either
 * would record silence or howl. It was written twice already before this file existed.
 *
 * The hands-free conversation in `data/conversation.ts` deliberately does NOT use this. It reads
 * frames as they arrive — a gate decides mid-recording when somebody stopped talking — where both
 * callers here only want the whole thing at the end. Same three nodes, genuinely different job.
 */

import { durationMs, encodeCapture } from "./audio";

/** A microphone that is open, and everything needed to close it again. */
export interface ActiveCapture {
  stream: MediaStream;
  context: AudioContext;
  source: MediaStreamAudioSourceNode;
  processor: ScriptProcessorNode;
  sink: GainNode;
  /** Raw mono frames, one `Float32Array` per `onaudioprocess` tick, joined only when it stops. */
  frames: Float32Array[];
}

/** Samples per `onaudioprocess` callback. Nothing downstream cares; this is the size both callers used. */
const BUFFER_SAMPLES = 4096;

/**
 * Opens the microphone and starts keeping what it hears.
 *
 * Throws whatever `getUserMedia` throws — a refused permission, a machine with no input — because
 * the two callers say different things about it and neither wants a swallowed failure.
 *
 * `constraints` is the caller's, not a default with an override: a dictation wants the device raw,
 * and anything recording while audio plays back needs `echoCancellation` or it records the answer.
 */
export async function startCapture(
  constraints: MediaTrackConstraints | boolean = true,
): Promise<ActiveCapture> {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: constraints });
  const AudioContextCtor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
  const context = new AudioContextCtor();
  const source = context.createMediaStreamSource(stream);
  const processor = context.createScriptProcessor(BUFFER_SAMPLES, 1, 1);
  // See the module header: zero gain keeps the graph live to `destination` without playing the
  // microphone out loud. No automated test can see this — jsdom has neither API.
  const sink = context.createGain();
  sink.gain.value = 0;

  const frames: Float32Array[] = [];
  processor.onaudioprocess = (event) => {
    frames.push(new Float32Array(event.inputBuffer.getChannelData(0)));
  };
  source.connect(processor);
  processor.connect(sink);
  sink.connect(context.destination);

  return { stream, context, source, processor, sink, frames };
}

/** What a finished recording is, once the graph is torn down: bytes to post, and how long they are. */
export interface FinishedCapture {
  bytes: Uint8Array;
  ms: number;
}

/**
 * Closes the microphone and encodes what it heard.
 *
 * The device rate is read BEFORE the context is closed — a closed `AudioContext` reports nothing,
 * and encoding at the wrong rate produces audio that plays at the wrong speed rather than an error.
 */
export async function finishCapture(active: ActiveCapture): Promise<FinishedCapture> {
  active.processor.disconnect();
  active.source.disconnect();
  active.sink.disconnect();
  for (const track of active.stream.getTracks()) track.stop();
  const deviceRate = active.context.sampleRate;
  await active.context.close();

  const interleaved = concatFrames(active.frames);
  return {
    bytes: encodeCapture(interleaved, 1, deviceRate),
    ms: durationMs(interleaved.length, deviceRate),
  };
}

/** Every chunk `onaudioprocess` handed over, joined into the one buffer `lib/audio.ts` expects. */
function concatFrames(frames: Float32Array[]): Float32Array {
  const total = frames.reduce((sum, chunk) => sum + chunk.length, 0);
  const out = new Float32Array(total);
  let offset = 0;
  for (const chunk of frames) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}
