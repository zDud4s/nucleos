/**
 * Turning what a microphone gives us into what the transcriber wants.
 *
 * This lives on the frontend rather than in the shell's Rust side, and not by preference. Capture has
 * to happen here — `cpal` cannot be built alongside Tauri right now — and once the samples exist in
 * the webview they cannot affordably be handed over: Tauri's IPC serialises arguments as JSON, so a
 * twenty-minute memo at 48 kHz would cross the boundary as roughly 58 million JSON numbers. The
 * daemon accepts WAV bytes over localhost HTTP, so whoever holds the samples should encode and POST
 * them, and that is here.
 *
 * The conversion existed in Rust first and was deliberately removed rather than left in place: two
 * implementations of one wire format drift, and the transcript is the thing that would quietly differ.
 */

/** What `core/src/voice.rs` sizes its body limit against, and what whisper wants. */
export const TARGET_SAMPLE_RATE = 16000;

/**
 * Averages interleaved frames down to one channel.
 *
 * Averaging rather than taking the first channel: on hardware that splits a capsule across both,
 * dropping one loses half the signal. A trailing partial frame is discarded, because a frame missing
 * channels is not a quieter frame — averaging it against absent samples puts a click at the end of
 * every recording.
 */
export function downmixToMono(interleaved: Float32Array, channels: number): Float32Array {
  if (channels <= 0) return new Float32Array(0);
  if (channels === 1) return interleaved;
  const frames = Math.floor(interleaved.length / channels);
  const out = new Float32Array(frames);
  for (let frame = 0; frame < frames; frame += 1) {
    let sum = 0;
    for (let channel = 0; channel < channels; channel += 1) {
      sum += interleaved[frame * channels + channel];
    }
    out[frame] = sum / channels;
  }
  return out;
}

/**
 * Linear resampling to the transcriber's rate.
 *
 * Linear rather than windowed-sinc: this is speech going into a model trained on 16 kHz, and the
 * aliasing a better filter removes sits above the band that model uses. A rate of zero yields nothing
 * rather than dividing by it — a device we cannot interpret must not be guessed at, because inventing
 * a rate silently pitch-shifts the recording.
 */
export function resampleLinear(input: Float32Array, fromHz: number, toHz: number): Float32Array {
  if (input.length === 0 || fromHz <= 0 || toHz <= 0) return new Float32Array(0);
  if (fromHz === toHz) return input;
  const outLength = Math.floor((input.length * toHz) / fromHz);
  if (outLength === 0) return new Float32Array(0);
  const ratio = fromHz / toHz;
  const last = input.length - 1;
  const out = new Float32Array(outLength);
  for (let i = 0; i < outLength; i += 1) {
    const position = i * ratio;
    const left = Math.floor(position);
    const fraction = position - left;
    const a = input[Math.min(left, last)];
    const b = input[Math.min(left + 1, last)];
    out[i] = a + (b - a) * fraction;
  }
  return out;
}

/**
 * Converts to signed 16-bit, clamping rather than wrapping.
 *
 * A sample above 1.0 happens — input gain, a shout, a device that does not normalise. Clamping is
 * audible clipping the model copes with; wrapping turns a loud vowel into full-scale noise of the
 * opposite sign, which is a different sound rather than a louder one. NaN becomes silence, because a
 * NaN sample has no correct integer.
 */
export function toPcm16(samples: Float32Array): Int16Array {
  const out = new Int16Array(samples.length);
  for (let i = 0; i < samples.length; i += 1) {
    const sample = samples[i];
    if (Number.isNaN(sample)) {
      out[i] = 0;
    } else {
      out[i] = Math.round(Math.max(-1, Math.min(1, sample)) * 32767);
    }
  }
  return out;
}

/** Wraps PCM in a canonical 44-byte WAV header. */
export function wavBytes(pcm: Int16Array, sampleRate: number): Uint8Array {
  const dataLength = pcm.length * 2;
  const buffer = new ArrayBuffer(44 + dataLength);
  const view = new DataView(buffer);
  const ascii = (at: number, text: string) => {
    for (let i = 0; i < text.length; i += 1) view.setUint8(at + i, text.charCodeAt(i));
  };

  ascii(0, "RIFF");
  // Everything after this field: the "WAVE" tag, the 24-byte fmt chunk, the 8-byte data header, then
  // the samples.
  view.setUint32(4, 36 + dataLength, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  view.setUint32(16, 16, true); // PCM fmt chunk length
  view.setUint16(20, 1, true); // format 1 = uncompressed PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true); // byte rate: rate * channels * 2
  view.setUint16(32, 2, true); // block align
  view.setUint16(34, 16, true); // bits per sample
  ascii(36, "data");
  view.setUint32(40, dataLength, true);
  for (let i = 0; i < pcm.length; i += 1) {
    view.setInt16(44 + i * 2, pcm[i], true);
  }
  return new Uint8Array(buffer);
}

/**
 * The recording as the transcriber's own audio: one channel, 16 kHz, still floating point.
 *
 * Split out of `encodeCapture` because this is the form anything that has to LOOK at the audio needs.
 * Silero is defined on 512-sample frames at 16 kHz, so a speech gate placed before the encoding reads
 * exactly these samples — and one placed before the resampling would be judging frames that mean a
 * different amount of time on every machine.
 */
export function monoAt16k(
  interleaved: Float32Array,
  channels: number,
  deviceRate: number,
): Float32Array {
  return resampleLinear(downmixToMono(interleaved, channels), deviceRate, TARGET_SAMPLE_RATE);
}

/** The whole conversion, in the order the hardware forces. */
export function encodeCapture(
  interleaved: Float32Array,
  channels: number,
  deviceRate: number,
): Uint8Array {
  return wavBytes(toPcm16(monoAt16k(interleaved, channels, deviceRate)), TARGET_SAMPLE_RATE);
}

/**
 * How long a capture lasted, from the frame count rather than from a clock read at each end.
 *
 * A clock would include the time spent opening the device and handing samples over, and the daemon
 * turns this number into a transcription deadline — so an inflated duration buys a slow transcriber
 * more time than the audio justifies, and a wrong one is invisible in the stored row.
 */
export function durationMs(frames: number, deviceRate: number): number {
  if (deviceRate <= 0) return 0;
  return Math.floor((frames * 1000) / deviceRate);
}
