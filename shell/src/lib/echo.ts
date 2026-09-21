/**
 * Decide whether a completed transcript segment is the assistant hearing itself.
 *
 * This defends against the microphone hearing the assistant's own answer after an imperfect echo
 * canceller lets it through and the transcriber hands it back as if the person had spoken. The
 * judge compares words, not audio, because those words are what would otherwise become a false turn.
 */

/**
 * Fold the spelling variations the transcriber commonly introduces before comparing words.
 *
 * This follows `fold_for_hint` in `core/src/voice.rs`: its measured Portuguese failure mode is
 * dropping an accent, so "amanha" must match "amanhã". Punctuation stays intact here; tokenisation
 * decides separately where words begin and end.
 */
export function fold(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

function tokens(text: string): string[] {
  return fold(text).split(/[^\p{L}\p{N}]+/u).filter(Boolean);
}

export const STOP_WORDS: ReadonlySet<string> = new Set([
  "o", "a", "os", "as", "um", "uma", "e", "ou", "de", "do", "da", "dos", "das", "em",
  "no", "na", "nos", "nas", "com", "que", "se", "ao", "aos", "por", "pelo", "pela", "eu",
  "tu", "ele", "ela", "me", "te", "lhe", "isso", "isto", "mais", "ja",
]);

/** Commands must always reach the assistant, even when they overlap its last answer. */
export const OVERRIDE_WORDS: ReadonlySet<string> = new Set([
  "stop", "espera", "cancela", "chega", "cala", "para", "pausa", "silencio",
]);

/**
 * One distinctive word is too little evidence: a real follow-up can be "e a segunda?", and
 * reusing one word from an answer is normal conversation. Eating the person's question is worse
 * than allowing one echo fragment through.
 */
export const MIN_DISTINCTIVE = 2;

/** Keep meaningful repeated words once, in the order the person said them. */
export function distinctive(text: string): string[] {
  const seen = new Set<string>();
  return tokens(text).filter((word) => {
    if (STOP_WORDS.has(word) || seen.has(word)) return false;
    seen.add(word);
    return true;
  });
}

/**
 * An explicit stop wins before overlap is considered: someone telling the assistant to stop means
 * it even if the answer used the same word, so no amount of overlap may swallow that command.
 */
export function isEcho(heard: string, spoken: string): boolean {
  const heardTokens = tokens(heard);
  if (heardTokens.some((word) => OVERRIDE_WORDS.has(word))) return false;

  const heardDistinctive = distinctive(heard);
  if (heardDistinctive.length < MIN_DISTINCTIVE) return false;

  const spokenTokens = new Set(tokens(spoken));
  return heardDistinctive.every((word) => spokenTokens.has(word));
}
