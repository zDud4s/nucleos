import type { ReactNode } from "react";
import { TERMS, type TermKey } from "../copy";
import { Tip } from "./Tip";

export interface TermProps {
  /** Which glossary entry this is. The definition lives in `copy.ts`, once. */
  term: TermKey;
  /** The words shown; defaults to the term itself. */
  children?: ReactNode;
}

/** A jargon word with its definition one focus, hover or press away. */
export function Term({ term, children }: TermProps) {
  return <Tip tip={TERMS[term]}>{children ?? term}</Tip>;
}
