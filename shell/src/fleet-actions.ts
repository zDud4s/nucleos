import { useState } from "react";

import { approveProposal, proposeExclusion, rejectProposal, revokeExclusion } from "./api";
import { type Partner } from "./fleet-derive";

/**
 * A card's part in picking a pair.
 *
 * `none` covers two different situations that need the same drawing — the owner is a run, or it is
 * the only job among its neighbours — and both mean the same thing to the reader: there is nothing
 * here to pair with.
 */
export type PairingRole = "none" | "offer" | "picking" | "target";

/**
 * Everything a view does about "these two must not run at the same time".
 *
 * It is a hook and not a module of plain functions because the *asking* has state: a request needs
 * two jobs and a card knows one, so the first click arms and the second asks. Two views now share
 * that — the column, where the pair is picked in two clicks, and the canvas, where it is one drag —
 * and the arming has to mean the same thing in both.
 *
 * Called once per view, not once per card: the arming is a property of the surface being looked at.
 */
export function useExclusionActions(token: string, refresh: () => Promise<void>) {
  // The job whose partner is being picked.
  const [pairing, setPairing] = useState<number | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  // Kept apart from `failed`, and drawn apart: the one thing it says is that an approval succeeded
  // and wrote no rule, which is not a failure and must not wear a failure's colours.
  const [closed, setClosed] = useState<string | null>(null);

  /**
   * What a card may do about pairing, which is never "whatever the others may do".
   *
   * The rule is that an action is only offered where it can SUCCEED. A run holds a slot but is not
   * a job; a job whose every neighbour it is already tied to has nobody left to ask about; and a
   * card already tied to the one being picked from cannot be its target. Each of those, offered
   * anyway, is a button whose only possible outcome is the daemon's 409 — and a person who clicks
   * it learns that the screen was showing them something that was never available.
   *
   * `jobIds` is an argument rather than something this hook knows: who the neighbours are is the
   * view's question, and the column and the canvas answer it differently.
   */
  function roleFor(jobId: number | null, partners: Partner[], jobIds: number[]): PairingRole {
    if (jobId === null) return "none";
    if (pairing === jobId) return "picking";
    const tied = new Set(partners.map((partner) => partner.partner));
    if (pairing !== null) return tied.has(pairing) ? "none" : "target";
    return jobIds.some((other) => other !== jobId && !tied.has(other)) ? "offer" : "none";
  }

  /**
   * Asks outright, with no arming step. This is what a drag does: the gesture already carries both
   * ends, so there is nothing to remember between two clicks.
   *
   * `setPairing(null)` runs whether or not anything was armed — the drag path never armed, and
   * clearing something already clear costs nothing.
   */
  async function ask(from: number, to: number) {
    setFailed(null);
    const outcome = await proposeExclusion(token, from, to);
    setPairing(null);
    // The daemon's own sentence, as `NewJob` does with `createJob`: the two 409s here mean opposite
    // things — wait for the approval, or stop clicking because it is already in force.
    if (!outcome.ok) {
      setFailed(outcome.reason);
      return;
    }
    await refresh();
  }

  /** The two-click gesture: the first card arms, the second is asked about. */
  function pick(jobId: number) {
    if (pairing === null) setPairing(jobId);
    else void ask(pairing, jobId);
  }

  /** Abandons the pick. Lives beside it because the card armed FROM can leave the view mid-pick. */
  function neverMind() {
    setPairing(null);
  }

  async function lift(id: number) {
    setFailed(null);
    if (!(await revokeExclusion(token, id))) {
      setFailed("That rule was already lifted, or the daemon did not answer.");
    }
    await refresh();
  }

  /**
   * The answer is given here and not on the Autopilot tab.
   *
   * That queue serves `action-approval` alone — approving one resumes a paused run, and approving
   * this resumes nothing — so the daemon keeps the two apart, as it already does for a contact
   * merge. It also puts the question where the context is: whether two jobs should be serialised is
   * decided while looking at them.
   */
  async function decide(proposalId: number, yes: boolean) {
    setFailed(null);
    setClosed(null);
    if (yes) {
      const outcome = await approveProposal(token, proposalId);
      if (!outcome.ok) setFailed(outcome.reason);
      // An approval that wrote no rule, because both jobs ended while the request waited. Said out
      // loud: the edge is about to disappear, and an edge that vanishes on the click that approved
      // it reads as a rule that was made, not as one that was no longer worth making.
      else if (outcome.closed !== null) setClosed(outcome.closed);
    } else if (!(await rejectProposal(token, proposalId))) {
      setFailed("That request could not be refused — it may already have been decided.");
    }
    await refresh();
  }

  return { pairing, failed, closed, roleFor, ask, pick, neverMind, lift, decide };
}
