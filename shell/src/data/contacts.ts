import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * The Contacts pillar: the roster read one address at a time, the identity
 * questions the heuristic raises about it, and the one write besides a
 * sender's verdict that belongs here — pulling two addresses back apart.
 *
 * **`GET /contacts` answers one row per address, deliberately the unmerged
 * view** (`http.rs:907-917`, `contacts.rs:485-531`). Two rows sharing one
 * `contact_id` ARE a merge; there is no separate "merged contact" shape to
 * read. No params, hard cap 200, ordered `messages_in DESC, last_seen DESC,
 * address`. Admin scope, like every route in this file.
 *
 * `MergeSide`, `MergeSuggestion` and {@link useContactMerges} used to live in
 * `data/waiting.ts` — moved here because they are Contacts data, and
 * re-exported from there under the idiom that file already uses for
 * `useProposals` and `useExclusionRequests`, so the Waiting queue's own
 * import keeps working unchanged.
 */

/** One address, exactly as `contacts::Correspondent` serialises. */
export interface Correspondent {
  address: string;
  /** Two rows sharing this id ARE a merge — there is no other signal for it. */
  contact_id: number;
  /** "human" | "implicit" — how THIS address was linked to its contact. */
  linked_by: string;
  display_name: string | null;
  messages_in: number;
  /** `i64` 0/1 over the wire, NOT a JSON boolean. */
  outbound_ever: number;
  first_seen: string;
  last_seen: string;
  /** "pin" | "mute" | null — a standing decision about this sender. */
  verdict: string | null;
}

/** How many rows `GET /contacts` returns at most. */
export const CONTACTS_LIST_LIMIT = 200;

/** The roster, one row per address. */
export function useContacts() {
  return useQuery({
    queryKey: keys.contacts.all,
    queryFn: () => apiFetch<Correspondent[]>("/contacts"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/** One side of a suggested merge, with the standing decision that could refuse it. */
export interface MergeSide {
  contact_id: number;
  addresses: string[];
  display_name: string | null;
  messages_in: number;
  /**
   * The standing decision on this person. Carried so the conflict that would
   * refuse the merge is visible BEFORE the button is pressed rather than as a
   * 409 afterwards.
   */
  verdict: string | null;
}

/** A pending suggestion that two contacts are one person. */
export interface MergeSuggestion {
  proposal_id: number;
  reasoning: string;
  created_at: string;
  /** The contact that survives — the lower id, which is the order the pair is keyed on. */
  keep: MergeSide;
  absorb: MergeSide;
}

/** The pairs the núcleo thinks are one person — `GET /contacts/merges`. */
export function useContactMerges() {
  return useQuery({
    queryKey: keys.contacts.merges,
    queryFn: () => apiFetch<MergeSuggestion[]>("/contacts/merges"),
    refetchInterval: POLL.queue,
    placeholderData: keepPreviousData,
  });
}

/** Two standing decisions that disagree — the 409 a merge decision can predict before it is made. */
export function mergeVerdictsConflict(suggestion: MergeSuggestion): boolean {
  const keep = suggestion.keep.verdict;
  const absorb = suggestion.absorb.verdict;
  return keep !== null && absorb !== null && keep !== absorb;
}

/**
 * Pull one address back out of a merge.
 *
 * `POST /contacts/unmerge` — verified (`http.rs:952-973`, `contacts.rs:372-420`).
 * Body is the address alone, not the contact: `unmerge` looks the contact up
 * from it. 204 on success, even when the address was never merged — the
 * route is idempotent by construction, not because "already apart" is worth
 * reporting as a refusal.
 *
 * **The daemon does NOT restrict this to human-made links** — `linked_by` is
 * never read by `unmerge` at all; `contacts.rs:491-493` says in so many words
 * that gating it is the caller's job. This hook trusts whatever address it is
 * given; `UnmergeButton` in `pages/Contacts.tsx` is the actual gate, and it
 * renders nothing for a link the heuristic made on its own.
 */
export function useUnmerge() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (address: string) =>
      apiFetch<void>("/contacts/unmerge", { method: "POST", body: JSON.stringify({ address }) }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.contacts.all });
    },
  });
}

/**
 * A standing decision about a sender — pin, mute, or clear.
 *
 * Not redeclared: `POST /contacts/verdict` already has a hook in
 * `data/mail.ts`, read from the message detail page, and it invalidates both
 * `mail` and `contacts` on success — the two namespaces the design splits
 * this one fact across. Re-exported so this page's import stays inside its
 * own pillar rather than reaching into Mail's module directly.
 */
export { useSenderVerdict, type SenderVerdictInput } from "./mail";
