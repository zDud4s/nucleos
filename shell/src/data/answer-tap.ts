/**
 * A per-frame read of the answer being spoken, for the voice room's drawing.
 *
 * The answer plays on an `HTMLAudioElement` owned by `data/conversation.ts`, and the microphone is
 * open while it plays. This tap listens to that element through `captureStream()` and NOT through
 * `createMediaElementSource()`: the second one reroutes the element's output into the Web Audio graph,
 * so playback would leave the path that echo cancellation reasons about. `captureStream()` only
 * observes — playback stays on the element, the play path is untouched, and the analyser is never
 * connected to the destination, so nothing is heard twice.
 *
 * Everything here degrades to silence. A webview without `captureStream` or `AudioContext`, a
 * suspended context or a stream with no audio yet reads as level 0 and no bands — a room that holds
 * still, never a conversation that breaks.
 *
 * The reads are meant to be called once per animation frame and are never React state.
 */

/** How many bars the answer's spectrum is folded into. */
export const ANSWER_BANDS = 24;

/** FFT window: 512 bins, enough resolution for 24 log-spaced bands from 80 Hz up. */
const FFT_SIZE = 1024;
const LOWEST_HZ = 80;
const HIGHEST_HZ = 8000;
/** Loudness range mapped onto 0..1: -60 dBFS reads as silence, -10 dBFS as full. */
const FLOOR_DB = -60;
const SPAN_DB = 50;

export interface AnswerTap {
  /** The answer's loudness right now, 0..1; 0 without a source. */
  level(): number;
  /** Fills `out` (ANSWER_BANDS long) low to high with 0..1; returns false, zero-filled, without a source. */
  bands(out: Float32Array): boolean;
}

/** What the tap reads where nothing is attached. */
export const SILENT_TAP: AnswerTap = {
  level: () => 0,
  bands(out) {
    out.fill(0);
    return false;
  },
};

type CapturableMedia = HTMLMediaElement & { captureStream?: () => MediaStream };

export function createAnswerTap(): {
  tap: AnswerTap;
  attach(el: HTMLMediaElement): void;
  detach(): void;
  close(): void;
} {
  let ctx: AudioContext | null = null;
  let analyser: AnalyserNode | null = null;
  let source: MediaStreamAudioSourceNode | null = null;
  let edges: number[] = [];
  let timeBuffer: Float32Array | null = null;
  let freqBuffer: Uint8Array | null = null;
  /** Bumped by every attach and detach, so a late `addtrack` cannot connect a stale stream. */
  let attachment = 0;

  function bandEdges(sampleRate: number, binCount: number): number[] {
    const result: number[] = [];
    for (let k = 0; k <= ANSWER_BANDS; k += 1) {
      const hz = LOWEST_HZ * (HIGHEST_HZ / LOWEST_HZ) ** (k / ANSWER_BANDS);
      let bin = Math.round((hz * FFT_SIZE) / sampleRate);
      bin = Math.min(Math.max(bin, 1), binCount - 1);
      // Every band is at least one bin wide, even where the low end would collapse several together.
      if (k > 0 && bin <= result[k - 1]) bin = result[k - 1] + 1;
      result.push(bin);
    }
    return result;
  }

  function connect(stream: MediaStream): void {
    if (ctx === null || analyser === null) return;
    source = ctx.createMediaStreamSource(stream);
    source.connect(analyser);
  }

  function detach(): void {
    attachment += 1;
    try {
      source?.disconnect();
    } catch {
      // Already disconnected; the tap reads silence either way.
    }
    source = null;
  }

  function attach(el: HTMLMediaElement): void {
    try {
      const capture = (el as CapturableMedia).captureStream;
      if (typeof capture !== "function") return;
      if (typeof window.AudioContext !== "function") return;

      if (ctx === null) ctx = new window.AudioContext();
      if (typeof ctx.createAnalyser !== "function") return;
      if (ctx.state === "suspended") void ctx.resume().catch(() => {});

      if (analyser === null) {
        analyser = ctx.createAnalyser();
        analyser.fftSize = FFT_SIZE;
        analyser.smoothingTimeConstant = 0.6;
        analyser.minDecibels = -90;
        analyser.maxDecibels = -20;
        edges = bandEdges(ctx.sampleRate, analyser.frequencyBinCount);
        timeBuffer = new Float32Array(analyser.fftSize);
        freqBuffer = new Uint8Array(analyser.frequencyBinCount);
      }

      detach();
      const mine = attachment;
      const stream = capture.call(el) as MediaStream;
      if (stream.getAudioTracks().length === 0) {
        // The element has not produced audio yet; connect when its first track appears.
        stream.addEventListener(
          "addtrack",
          () => {
            if (attachment !== mine) return;
            try {
              connect(stream);
            } catch {
              // Degrade silently.
            }
          },
          { once: true },
        );
        return;
      }
      connect(stream);
    } catch {
      // Any failure leaves the tap silent rather than breaking playback.
    }
  }

  function close(): void {
    detach();
    void ctx?.close();
    ctx = null;
    analyser = null;
  }

  const tap: AnswerTap = {
    level() {
      if (source === null || analyser === null || timeBuffer === null) return 0;
      analyser.getFloatTimeDomainData(timeBuffer);
      let sum = 0;
      for (let i = 0; i < timeBuffer.length; i += 1) sum += timeBuffer[i] * timeBuffer[i];
      const rms = Math.sqrt(sum / timeBuffer.length);
      const db = 20 * Math.log10(Math.max(rms, 1e-8));
      return Math.min(Math.max((db - FLOOR_DB) / SPAN_DB, 0), 1);
    },
    bands(out) {
      if (source === null || analyser === null || freqBuffer === null) {
        out.fill(0);
        return false;
      }
      analyser.getByteFrequencyData(freqBuffer);
      for (let k = 0; k < ANSWER_BANDS; k += 1) {
        const from = edges[k];
        const to = edges[k + 1];
        let sum = 0;
        for (let bin = from; bin < to; bin += 1) sum += freqBuffer[bin];
        out[k] = sum / (to - from) / 255;
      }
      return true;
    },
  };

  return { tap, attach, detach, close };
}
