// §spec correspondent-contacts
import { useId, useState, type KeyboardEvent, type ReactNode } from "react";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  mergeVerdictsConflict,
  useContactMerges,
  useContacts,
  useSenderVerdict,
  useUnmerge,
  type Correspondent,
  type MergeSide,
  type MergeSuggestion,
} from "../data/contacts";
import { useDecideContactMerge, type ApprovalOutcome } from "../data/waiting";
import {
  Badge,
  Button,
  ConfirmButton,
  ConflictNote,
  Count,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Section,
  StaleNote,
} from "../ui";
import "./contacts.css";

/**
 * Contacts — the roster, one row per address, and the identity questions the
 * heuristic raises about it.
 *
 * Two facts from `data/contacts.ts` shape everything below. **`GET /contacts`
 * is deliberately the unmerged view**: two rows sharing one `contact_id` ARE
 * a merge, and there is no second, already-joined shape to read instead — the
 * §6.13 badges (`urgent`, `noise`, `you write back`, `merged`) are all
 * derived from the raw row, never from a daemon-side rollup. And **merge
 * decisions go through the ordinary proposal doors**, `/proposals/{id}
 * /approve|reject` — there is no `/contacts/merges/{id}/approve` — which is
 * why {@link IdentityQuestions} reuses `useDecideContactMerge` from the
 * Waiting queue rather than inventing a second decision path for the same
 * two routes.
 */

function headline(rows: Correspondent[] | undefined): string | undefined {
  if (rows === undefined) return undefined;
  if (rows.length === 0) return "no one has written in yet";
  const shared = mergedContactIds(rows);
  const noun = rows.length === 1 ? "address" : "addresses";
  const first = `${rows.length} ${noun}`;
  // Count addresses and not contacts: the number sits beside the address total, and
  // "2 sharing" next to "7 addresses" read as two addresses when it was two people.
  const sharing = rows.filter((row) => shared.has(row.contact_id)).length;
  return sharing === 0 ? first : `${first}, ${sharing} belonging to someone with another address`;
}

export function Contacts() {
  const contacts = useContacts();
  const merges = useContactMerges();
  const rows = contacts.data;
  const stale = contacts.isError && rows !== undefined;

  return (
    <>
      <PageHeader title="Contacts" headline={headline(rows)} />

      <IdentityQuestions view={merges} />

      {rows !== undefined && rows.length === 0 ? (
        <Section label="People">
          <QuietLine says="no one has written in yet">
            Every address that has sent or received mail lands here, one row per address.
          </QuietLine>
        </Section>
      ) : (
        <Panel title="People" aside={<Count n={rows?.length} />}>
          {stale && <StaleNote dataUpdatedAt={contacts.dataUpdatedAt} />}
          {contacts.isError && rows === undefined && <RosterError error={contacts.error} />}
          {rows === undefined && !contacts.isError && <p className="contacts-loading">reading the roster…</p>}
          {rows !== undefined && <PeopleRoster rows={rows} />}
        </Panel>
      )}
    </>
  );
}

/** Which contact ids appear more than once in this listing — the merged pairs. */
function mergedContactIds(rows: Correspondent[]): Set<number> {
  const counts = new Map<number, number>();
  for (const row of rows) counts.set(row.contact_id, (counts.get(row.contact_id) ?? 0) + 1);
  const merged = new Set<number>();
  for (const [id, count] of counts) {
    if (count > 1) merged.add(id);
  }
  return merged;
}

function RosterError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the roster</ErrorNote>;
}

/**
 * A run of text whose reasoning is drawn as a tip under it on hover or focus.
 * No "why?" button and no paragraph unfolding beside it — the owner's call
 * (2026-10-09). The tip stays in the document as the text's description, so a
 * screen reader still hears it. Escape drops focus, which is how a keyboard
 * user dismisses it.
 */
function Tip({ says, children }: { says: ReactNode; children: ReactNode }) {
  const tipId = useId();
  const dismiss = (event: KeyboardEvent<HTMLSpanElement>) => {
    if (event.key === "Escape") event.currentTarget.blur();
  };
  return (
    <span className="contacts-quiet-says" tabIndex={0} aria-describedby={tipId} onKeyDown={dismiss}>
      {says}
      <span className="contacts-quiet-tip" role="tooltip" id={tipId}>
        <span className="contacts-quiet-tip-box">{children}</span>
      </span>
    </span>
  );
}

/** An empty section's one line, with its reasoning in a {@link Tip}. */
function QuietLine({ says, children }: { says: string; children: ReactNode }) {
  return (
    <div className="ui-quiet">
      <p>
        <Tip says={says}>{children}</Tip>
      </p>
    </div>
  );
}

/* -------------------------------------------------------- identity questions -- */

/**
 * Two records the heuristic thinks are one person, decided through the same
 * approve and reject doors as everything else that lands in the Waiting
 * queue.
 *
 * Rejecting here is PERMANENT — `reject_merge` records the refused pair in
 * the same transaction as the status, so the identical question is never
 * asked again — and that is said once, above the list, before either button
 * is ever pressed, not discovered afterwards.
 */
function IdentityQuestions({ view }: { view: ReturnType<typeof useContactMerges> }) {
  const decide = useDecideContactMerge();
  const rows = view.data ?? [];

  if (view.data !== undefined && rows.length === 0) {
    return (
      <Section label="Identity questions">
        <QuietLine says="nothing looks like the same person">
          The heuristic sweeps the roster on its own; an empty list is it finding no match, not a
          sign anything here is stuck.
        </QuietLine>
      </Section>
    );
  }

  if (view.data === undefined && !view.isError) {
    return (
      <Panel title="Identity questions" aside={<Count n={undefined} />}>
        <p className="contacts-loading">reading the suggestions…</p>
      </Panel>
    );
  }

  return (
    <Panel title="Identity questions" aside={<Count n={view.data?.length} />}>
      <p className="contacts-note">
        Two records that may be one person. Yes merges them — undo it per address in People below.
        No is permanent: this pair is never suggested again.
      </p>
      {view.isError && rows.length === 0 && <MergesError error={view.error} />}
      {rows.length > 0 && (
        <Rows label="Identity questions" className="contacts-questions">
          {rows.map((suggestion) => (
            <IdentityQuestion key={suggestion.proposal_id} suggestion={suggestion} decide={decide} />
          ))}
        </Rows>
      )}
      <DecisionNotes outcome={decide.data} error={decide.isError ? decide.error : null} />
    </Panel>
  );
}

function MergesError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the suggested merges</ErrorNote>;
}

function IdentityQuestion({
  suggestion,
  decide,
}: {
  suggestion: MergeSuggestion;
  decide: ReturnType<typeof useDecideContactMerge>;
}) {
  const conflict = mergeVerdictsConflict(suggestion);
  const noteId = useId();
  return (
    <Row className="contacts-question">
      <div className="contacts-question-head">
        <span className="contacts-question-who">
          {sideName(suggestion.keep)} ↔ {sideName(suggestion.absorb)}
        </span>
        <RelativeTime at={suggestion.created_at} />
      </div>
      <p className="contacts-reasoning">{suggestion.reasoning}</p>
      <div className="contacts-merge">
        <MergeSideView side={suggestion.keep} role="kept" />
        <MergeSideView side={suggestion.absorb} role="absorbed" />
      </div>
      {conflict && (
        <div id={noteId}>
          <ConflictNote>
            These two carry standing decisions that disagree, so approving will be refused — settle one
            of them and decide this again.
          </ConflictNote>
        </div>
      )}
      <div className="contacts-actions">
        <ConfirmButton
          label="Yes, one person"
          confirmLabel="They are one person"
          variant="approve"
          disabled={decide.isPending}
          unavailable={conflict}
          describedBy={conflict ? noteId : undefined}
          onConfirm={() => decide.mutate({ proposalId: suggestion.proposal_id, verdict: "approve" })}
        />
        <ConfirmButton
          label="No, different people"
          confirmLabel="Refuse permanently"
          variant="ghost"
          disabled={decide.isPending}
          onConfirm={() => decide.mutate({ proposalId: suggestion.proposal_id, verdict: "reject" })}
        />
      </div>
    </Row>
  );
}

/** What a side is called in a question's head: its display name, else its first address. */
function sideName(side: MergeSide): string {
  return side.display_name ?? side.addresses[0] ?? "no name recorded";
}

/**
 * One side of a suggested merge, under the word for what happens to it.
 *
 * `kept` and `absorbed` are a heading and not a field label — the whole point
 * of showing both sides is that the merge is not symmetrical, and which of the
 * two survives is the first thing a reader has to know. `Section` is the
 * heading rank; the label rank it used to be written in is what a `dt` gets,
 * and at 11px the two are told apart by 0.06em of tracking and nothing else.
 *
 * `level={3}` because the `Panel` above already spends the `h2` on "Identity
 * questions". Announced as an `h2` these would be that panel's siblings, which
 * is the opposite of what the page means.
 */
function MergeSideView({ side, role }: { side: MergeSide; role: string }) {
  return (
    <Section label={role} level={3}>
      <div className="contacts-side">
        <p className="contacts-side-name">{side.display_name ?? "no name recorded"}</p>
        <ul className="contacts-side-addresses">
          {side.addresses.map((address) => (
            <li key={address}>{address}</li>
          ))}
        </ul>
        <p className="contacts-meta">{side.messages_in} message{side.messages_in === 1 ? "" : "s"} in</p>
      </div>
    </Section>
  );
}

/**
 * Sentences for the two decision doors, page copy over the shared floor.
 *
 * The 409 is the one worth writing for: `reject_merge` and `approve_merge`
 * both guard on the pair's own standing decisions disagreeing, and the
 * daemon's own sentence names which two — see `daemonProse` below, which is
 * what lets that exact wording through instead of this generic fallback.
 */
const IDENTITY_SENTENCES: Record<string, string> = {
  conflict: "someone already answered this — the list clears it on the next read",
  not_found: "that question is gone; there is nothing left to decide",
  unprocessable: "the núcleo found nothing usable in this suggestion to act on",
  internal: "the núcleo failed while recording the decision — nothing changed",
};

/**
 * The daemon's own sentence, when it really sent one — `RunDetail.tsx`'s
 * pattern: a refusal under four words is a status word repeated, not prose
 * written on purpose, so only a longer one is trusted over this page's own
 * copy.
 */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

function DecisionNotes({ outcome, error }: { outcome: ApprovalOutcome | undefined; error: unknown }) {
  return (
    <>
      {outcome !== undefined && (
        <p className="contacts-outcome" role="status">
          {outcome.merged === true ? "the two are one person now" : "refused — this pair won't be suggested again"}
        </p>
      )}
      {error !== null && <IdentityRefusal error={error} />}
    </>
  );
}

function IdentityRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>the núcleo did not answer — nothing was decided</ErrorNote>;
  const sentences = { ...IDENTITY_SENTENCES, ...daemonProse(error) };
  return <RefusalNote refusal={error} sentences={sentences} />;
}

/* --------------------------------------------------------------- roster -- */

/** A mutation that failed on one card: whose address, and the note that says why. */
type Problem = { address: string; node: ReactNode };

/**
 * Tells the roster a card's mutation failed (`node`) or went through (`null`).
 * The note is drawn once above the grid, never inside the card, so a refusal
 * cannot grow one card and break the equal heights.
 */
type ReportProblem = (address: string, node: ReactNode | null) => void;

/** Rows with the addresses of one contact side by side, each contact where it first appears. */
function groupByContact(rows: Correspondent[]): Correspondent[] {
  const groups = new Map<number, Correspondent[]>();
  for (const row of rows) {
    const group = groups.get(row.contact_id);
    if (group === undefined) groups.set(row.contact_id, [row]);
    else group.push(row);
  }
  return [...groups.values()].flat();
}

function PeopleRoster({ rows }: { rows: Correspondent[] }) {
  const [problem, setProblem] = useState<Problem | null>(null);
  const report: ReportProblem = (address, node) => setProblem(node === null ? null : { address, node });
  const ordered = groupByContact(rows);
  // A grid of profile cards, one per address — the owner's call (2026-10-02):
  // the roster is browsed for people, not scanned down like the mail queue.
  // Each card is the inset rank, the one card a Panel is allowed to hold.
  return (
    <>
      {problem !== null && (
        <div role="status" className="contacts-problem">
          {problem.address} — {problem.node}
        </div>
      )}
      <ul className="contacts-cards" aria-label="People">
        {ordered.map((row) => (
          <PersonCard
            key={row.address}
            row={row}
            sameAs={ordered
              .filter((other) => other.contact_id === row.contact_id && other.address !== row.address)
              .map((other) => other.address)}
            onProblem={report}
          />
        ))}
      </ul>
    </>
  );
}

/** Up to two letters for the avatar: the name's initials, else the address's first letter. */
function initials(row: Correspondent): string {
  const words = (row.display_name ?? "").trim().split(/\s+/).filter((word) => word !== "");
  if (words.length >= 2) return (words[0][0] + words[words.length - 1][0]).toUpperCase();
  if (words.length === 1) return words[0].slice(0, 2).toUpperCase();
  return row.address.slice(0, 1).toUpperCase();
}

function PersonCard({
  row,
  sameAs,
  onProblem,
}: {
  row: Correspondent;
  /** The other addresses of this contact — empty unless the row is merged. */
  sameAs: string[];
  onProblem: ReportProblem;
}) {
  const merged = sameAs.length > 0;
  return (
    <li className="ui-panel-inset contacts-card">
      <div className="contacts-card-head">
        <span className="contacts-avatar" aria-hidden="true">
          {initials(row)}
        </span>
        <div className="contacts-card-who">
          <span className="contacts-card-name" title={row.display_name ?? undefined}>
            {row.display_name ?? "no name recorded"}
          </span>
          <span className="contacts-card-address">{row.address}</span>
        </div>
      </div>
      {/* §6.13's badges, every one derived from this one row — there is no
          daemon-side rollup to read any of them off instead. */}
      {/* The unmerge rides beside the badges, not in the action row: an extra
          control there wrapped to a second line and lifted that card's Pin
          above every other card's (owner's call, 2026-10-09). */}
      {(row.verdict !== null || row.outbound_ever === 1 || merged || row.linked_by === "human") && (
        <div className="contacts-card-badges">
          {row.verdict === "pin" && <Badge tone="pending">urgent</Badge>}
          {row.verdict === "mute" && <Badge tone="off">noise</Badge>}
          {row.outbound_ever === 1 && <Badge tone="info">you write back</Badge>}
          {merged && (
            <Badge tone="shadow">
              <Tip says="merged">same person as {sameAs.join(", ")}</Tip>
            </Badge>
          )}
          <UnmergeButton address={row.address} linkedBy={row.linked_by} onProblem={onProblem} />
        </div>
      )}
      <p className="contacts-meta">
        {row.messages_in} message{row.messages_in === 1 ? "" : "s"} in · last seen{" "}
        <RelativeTime at={row.last_seen} /> · first seen{" "}
        <RelativeTime at={row.first_seen} />
      </p>
      <div className="contacts-card-actions">
        <SenderVerdictToggles address={row.address} verdict={row.verdict} onProblem={onProblem} />
      </div>
    </li>
  );
}

/* -------------------------------------------------------- sender verdict -- */

function SenderVerdictToggles({
  address,
  verdict,
  onProblem,
}: {
  address: string;
  verdict: string | null;
  onProblem: ReportProblem;
}) {
  const mutate = useSenderVerdict();
  const send = (next: "pin" | "mute" | null) =>
    mutate.mutate(
      { address, verdict: next },
      {
        onSuccess: () => onProblem(address, null),
        onError: (error) => onProblem(address, <VerdictError error={error} />),
      },
    );
  // `aria-pressed` and not a variant swap: a button that changes weight when pressed
  // changes the card's metrics, and the "urgent" / "noise" badge already says it.
  return (
    <div className="contacts-actions">
      <Button variant="ghost" aria-pressed={verdict === "pin"} disabled={mutate.isPending} onClick={() => send("pin")}>
        Pin
      </Button>
      <Button aria-pressed={verdict === "mute"} disabled={mutate.isPending} onClick={() => send("mute")}>
        Mute
      </Button>
      <Button disabled={mutate.isPending || verdict === null} onClick={() => send(null)}>
        Clear
      </Button>
    </div>
  );
}

function VerdictError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return <RefusalNote refusal={error} sentences={{ not_found: "no mail has ever arrived from that address" }} />;
  }
  return <ErrorNote>the núcleo did not answer — that verdict was not recorded</ErrorNote>;
}

/* ------------------------------------------------------------- unmerge -- */

/**
 * "Not the same person" — offered only when THIS address's own link to its
 * contact was made by a human.
 *
 * **The daemon does not check `linked_by` before honouring `/contacts
 * /unmerge`** — `contacts.rs:491-493` says gating it is the caller's job.
 * Rendering nothing for a link the heuristic made on its own, rather than
 * trusting the door to refuse it, is what stops a person from pulling apart a
 * connection nobody actually decided.
 */
function UnmergeButton({
  address,
  linkedBy,
  onProblem,
}: {
  address: string;
  linkedBy: string;
  onProblem: ReportProblem;
}) {
  const unmerge = useUnmerge();
  if (linkedBy !== "human") return null;

  return (
    <div className="contacts-actions contacts-unmerge">
      <ConfirmButton
        label="Not the same person"
        confirmLabel="Pull this address back apart"
        variant="quiet"
        disabled={unmerge.isPending}
        onConfirm={() =>
          unmerge.mutate(address, {
            onSuccess: () => onProblem(address, null),
            onError: (error) => onProblem(address, <UnmergeError error={error} />),
          })
        }
      />
    </div>
  );
}

function UnmergeError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return <RefusalNote refusal={error} sentences={{ not_found: "that address is no longer in the roster" }} />;
  }
  return <ErrorNote>the núcleo did not answer — that address was not pulled apart</ErrorNote>;
}
