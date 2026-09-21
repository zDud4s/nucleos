import { describe, expect, it } from "vitest";

import {
  distinctive,
  fold,
  isEcho,
  MIN_DISTINCTIVE,
  OVERRIDE_WORDS,
  STOP_WORDS,
} from "./echo";

const LISBON_FORECAST = "Amanhã em Lisboa vai estar sol, com máximas de vinte graus.";

describe("the echo judge", () => {
  it("folding removes case and accents and nothing else", () => {
    expect(fold("Núcleo SILÊNCIO ação")).toBe("nucleo silencio acao");
    expect(fold("Olá, Mundo!")).toBe("ola, mundo!");
  });

  it("distinctive words drop function words, repeat once, keep their order", () => {
    expect(distinctive("O tempo em Lisboa e o tempo no Porto")).toEqual(["tempo", "lisboa", "porto"]);
    expect(distinctive("e a")).toEqual([]);
  });

  it("the stop and override lists do not overlap, and the override list is exactly the one fixed", () => {
    const namedStopWords = [
      "o", "a", "os", "as", "um", "uma", "e", "ou", "de", "do", "da", "dos", "das", "em", "no", "na",
      "nos", "nas", "com", "que", "se", "ao", "aos", "por", "pelo", "pela", "eu", "tu", "ele", "ela", "me",
      "te", "lhe", "isso", "isto", "mais", "ja",
    ];
    const fixedOverrideWords = ["stop", "espera", "cancela", "chega", "cala", "para", "pausa", "silencio"];

    for (const word of namedStopWords) expect(STOP_WORDS.has(word)).toBe(true);
    for (const word of OVERRIDE_WORDS) expect(STOP_WORDS.has(word)).toBe(false);
    expect([...OVERRIDE_WORDS].sort()).toEqual(fixedOverrideWords.sort());
  });

  it("a segment made only of the answer's own words is an echo", () => {
    expect(isEcho("lisboa vai estar sol", LISBON_FORECAST)).toBe(true);
    expect(isEcho("amanha em lisboa", LISBON_FORECAST)).toBe(true);
  });

  it("one word the answer never said makes it the person's turn", () => {
    expect(isEcho("e em Madrid vai estar sol", LISBON_FORECAST)).toBe(false);
  });

  it("a one-word follow-up is never an echo, even when the answer used that word", () => {
    expect(MIN_DISTINCTIVE).toBe(2);
    expect(isEcho("e a segunda?", "Na segunda vou tratar disso.")).toBe(false);
  });

  it("an override word always gets through", () => {
    expect(isEcho("espera, lisboa vai estar sol", LISBON_FORECAST)).toBe(false);
    expect(isEcho("cala-te", LISBON_FORECAST)).toBe(false);
    expect(isEcho("PARA com isso", LISBON_FORECAST)).toBe(false);
    expect(isEcho("espera pausa", "Não vou parar, espera um pouco pela pausa.")).toBe(false);
  });

  it("nothing to compare against is never an echo", () => {
    expect(isEcho("lisboa vai estar sol", "")).toBe(false);
    expect(isEcho("", LISBON_FORECAST)).toBe(false);
  });
});
