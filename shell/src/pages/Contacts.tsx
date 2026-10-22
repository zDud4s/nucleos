// §spec correspondent-contacts
import { useId, useState, type ReactNode } from "react";
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
import { DAEMON_NAME } from "../copy";
import { plural } from "../lib/plural";
import { useDecideContactMerge, type ApprovalOutcome } from "../data/waiting";
import {
  Badge,
  Button,
  ConfirmButton,
  ConflictNote,
  Count,
  ErrorNote,
  ListFilter,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Section,
  Skeleton,
  StaleNote,
  useUndoToast,
  type UndoToastOptions,
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
  const first = plural(rows.length, "address", "addresses");
  // Count addresses and not contacts: the number sits beside the address total, and
  // "2 sharing" next to "7 addresses" read as two addresses when it was two people.
  const sharing = rows.filter((row) => shared.has(row.contact_id)).length;
  return sharing === 0 ? first : `${first}, ${sharing} shared with another address`;
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
          {rows === undefined && !contacts.isError && <Skeleton label="Loading the roster" lines={3} />}
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
  return <ErrorNote>The {DAEMON_NAME} is not responding: nothing is known about the roster</ErrorNote>;
}

/**
 * An empty section's one line: the state, then its reason, both in view.
 * The reason used to sit in a hover tip; touch has no hover and keyboard
 * users found it by accident, so it is plain text now (critique #19).
 */
function QuietLine({ says, children }: { says: string; children: ReactNode }) {
  return (
    <div className="ui-quiet">
      <p>
        {says}. {children}
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
  // Who the last decision was about: the outcome itself carries no names. A new
  // decision resets `decide.data`, which clears the receipt below.
  const [decided, setDecided] = useState<string | null>(null);
  const rows = view.data ?? [];

  if (view.data !== undefined && rows.length === 0) {
    return (
      <Section label="Identity questions">
        <QuietLine says="nothing looks like the same person">
          The matcher sweeps the roster on its own; an empty list is it finding no match, not a
          sign anything here is stuck.
        </QuietLine>
      </Section>
    );
  }

  if (view.data === undefined && !view.isError) {
    return (
      <Panel title="Identity questions" aside={<Count n={undefined} />}>
        <Skeleton label="Loading the suggestions" lines={2} />
      </Panel>
    );
  }

  return (
    <Panel title="Identity questions" aside={<Count n={view.data?.length} />}>
      <p className="contacts-note">
        Two records that may be one person. Yes merges them (undo it per address in People below).
        No is permanent: this pair is never suggested again.
      </p>
      {view.isError && rows.length === 0 && <MergesError error={view.error} />}
      {rows.length > 0 && (
        <Rows label="Identity questions" className="contacts-questions">
          {rows.map((suggestion) => (
            <IdentityQuestion
              key={suggestion.proposal_id}
              suggestion={suggestion}
              decide={decide}
              onDecide={setDecided}
            />
          ))}
        </Rows>
      )}
      <DecisionNotes outcome={decide.data} who={decided} error={decide.isError ? decide.error : null} />
    </Panel>
  );
}

function MergesError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>The {DAEMON_NAME} is not responding: nothing is known about the suggested merges</ErrorNote>;
}

function IdentityQuestion({
  suggestion,
  decide,
  onDecide,
}: {
  suggestion: MergeSuggestion;
  decide: ReturnType<typeof useDecideContactMerge>;
  onDecide: (who: string) => void;
}) {
  const conflict = mergeVerdictsConflict(suggestion);
  const noteId = useId();
  const [keepName, absorbName] = pairNames(suggestion.keep, suggestion.absorb);
  const who = `${keepName} and ${absorbName}`;
  const send = (verdict: "approve" | "reject") => {
    onDecide(who);
    decide.mutate({ proposalId: suggestion.proposal_id, verdict });
  };
  // The head is the question the two buttons answer — "Yes, one person" and "No,
  // different people" had nothing on screen asking them. Which side stays and
  // which is merged in is said once, in the headings over the two sides; the
  // head used to say it a second time in parentheses.
  return (
    <Row className="contacts-question">
      <div className="contacts-question-head">
        <span className="contacts-question-who">
          Are {keepName} and {absorbName} one person?
        </span>
        <RelativeTime at={suggestion.created_at} />
      </div>
      <p className="contacts-reasoning">{suggestion.reasoning}</p>
      <div className="contacts-merge">
        <MergeSideView side={suggestion.keep} role="Stays" />
        <MergeSideView side={suggestion.absorb} role="Merged in" />
      </div>
      {conflict && (
        <div id={noteId} className="contacts-conflict">
          <ConflictNote>
            These two carry standing decisions that disagree, so approving will be refused. Settle one
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
          onConfirm={() => send("approve")}
        />
        <ConfirmButton
          label="No, different people"
          confirmLabel="Refuse permanently"
          variant="ghost"
          disabled={decide.isPending}
          onConfirm={() => send("reject")}
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
 * The two names a question asks about — by address when the names alone are the
 * same, since "Are Ana and Ana one person?" asks nothing a reader can answer.
 */
function pairNames(keep: MergeSide, absorb: MergeSide): [string, string] {
  const first = sideName(keep);
  const second = sideName(absorb);
  if (first !== second) return [first, second];
  return [keep.addresses[0] ?? first, absorb.addresses[0] ?? second];
}

/**
 * One side of a suggested merge, under the word for what happens to it.
 *
 * "Stays" and "Merged in" are a heading and not a field label — the whole point
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
        <p className={side.display_name === null ? "contacts-side-name contacts-missing" : "contacts-side-name"}>
          {side.display_name ?? "no name recorded"}
        </p>
        <ul className="contacts-side-addresses">
          {side.addresses.map((address) => (
            <li key={address}>{address}</li>
          ))}
        </ul>
        <p className="contacts-meta">{plural(side.messages_in, "message")} in</p>
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
  conflict: "someone already answered this, and the list clears it on the next read",
  not_found: "that question is gone; there is nothing left to decide",
  unprocessable: `the ${DAEMON_NAME} found nothing usable in this suggestion to act on`,
  internal: `the ${DAEMON_NAME} failed while recording the decision: nothing changed`,
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

function DecisionNotes({
  outcome,
  who,
  error,
}: {
  outcome: ApprovalOutcome | undefined;
  who: string | null;
  error: unknown;
}) {
  const names = who ?? "the two";
  return (
    <>
      {outcome !== undefined && (
        <p className="contacts-outcome" role="status">
          {outcome.merged === true
            ? `${names} are one person now. To undo it, use "Not the same person" on the address in People below.`
            : `Refused: ${names} won't be suggested again.`}
        </p>
      )}
      {error !== null && <IdentityRefusal error={error} />}
    </>
  );
}

function IdentityRefusal({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) return <ErrorNote>The {DAEMON_NAME} is not responding: nothing was decided</ErrorNote>;
  const sentences = { ...IDENTITY_SENTENCES, ...daemonProse(error) };
  return <RefusalNote refusal={error} sentences={sentences} />;
}

/* --------------------------------------------------------------- roster -- */

/**
 * Tells the card that a mutation on it failed (`node`) or went through (`null`).
 * Each card owns its own note, drawn inside it, so the person who pressed Mute
 * sees the refusal where they pressed and a second failure elsewhere cannot
 * overwrite it (critique #17).
 */
type ReportProblem = (node: ReactNode | null) => void;

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

type RosterFilter = "urgent" | "noise" | "merged" | "writeback";

const ROSTER_FILTERS: ReadonlyArray<{ key: RosterFilter; label: string }> = [
  { key: "urgent", label: "Urgent" },
  { key: "noise", label: "Noise" },
  { key: "merged", label: "Merged" },
  { key: "writeback", label: "You write back" },
];

/** Cards drawn before "Show more": the roster only grows, and every card is a tree of controls. */
const PAGE_SIZE = 48;

function matchesFilter(row: Correspondent, filter: RosterFilter, merged: Set<number>): boolean {
  switch (filter) {
    case "urgent":
      return row.verdict === "pin";
    case "noise":
      return row.verdict === "mute";
    case "merged":
      return merged.has(row.contact_id);
    case "writeback":
      return row.outbound_ever === 1;
  }
}

function PeopleRoster({ rows }: { rows: Correspondent[] }) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<RosterFilter[]>([]);
  const [shown, setShown] = useState(PAGE_SIZE);
  const undo = useUndoToast();
  const ordered = groupByContact(rows);
  const merged = mergedContactIds(rows);
  const needle = query.trim().toLowerCase();
  const visible = ordered.filter(
    (row) =>
      // Chips widen, they do not stack: "Urgent" plus "Noise" is both groups, none selected is everyone.
      (selected.length === 0 || selected.some((key) => matchesFilter(row, key, merged))) &&
      (needle === "" ||
        row.address.toLowerCase().includes(needle) ||
        (row.display_name ?? "").toLowerCase().includes(needle)),
  );
  const drawn = visible.slice(0, shown);
  const chips = ROSTER_FILTERS.map(({ key, label }) => ({
    id: key,
    label,
    count: ordered.filter((row) => matchesFilter(row, key, merged)).length,
  }));
  // A grid of profile cards, one per address — the owner's call (2026-10-02):
  // the roster is browsed for people, not scanned down like the mail queue.
  // Each card is the inset rank, the one card a Panel is allowed to hold.
  return (
    <>
      <div className="contacts-filter">
        <ListFilter
          noun="people"
          query={query}
          onQueryChange={(next) => {
            setQuery(next);
            setShown(PAGE_SIZE);
          }}
          chips={chips}
          selected={selected}
          onToggleChip={(id) => {
            setSelected((now) =>
              now.includes(id as RosterFilter)
                ? now.filter((key) => key !== id)
                : [...now, id as RosterFilter],
            );
            setShown(PAGE_SIZE);
          }}
          resultCount={visible.length}
          onClear={() => {
            setQuery("");
            setSelected([]);
            setShown(PAGE_SIZE);
          }}
        />
      </div>
      {visible.length === 0 ? (
        <p className="contacts-note">No one matches{needle === "" ? "" : ` "${query.trim()}"`}.</p>
      ) : (
        <ul className="contacts-cards" aria-label="People">
          {drawn.map((row) => (
            <PersonCard
              key={row.address}
              row={row}
              onVerdictSet={undo.show}
              sameAs={ordered
                .filter((other) => other.contact_id === row.contact_id && other.address !== row.address)
                .map((other) => other.address)}
            />
          ))}
        </ul>
      )}
      {visible.length > drawn.length && (
        <div className="contacts-more">
          <Button onClick={() => setShown(shown + PAGE_SIZE)}>
            Show more ({visible.length - drawn.length} left)
          </Button>
        </div>
      )}
      {undo.toast}
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
  onVerdictSet,
}: {
  row: Correspondent;
  /** Opens the page's undo window after a verdict lands. */
  onVerdictSet: (options: UndoToastOptions) => void;
  /** The other addresses of this contact — empty unless the row is merged. */
  sameAs: string[];
}) {
  const merged = sameAs.length > 0;
  const [problem, setProblem] = useState<ReactNode | null>(null);
  const onProblem: ReportProblem = setProblem;
  return (
    <li className="ui-panel-inset contacts-card">
      <div className="contacts-card-head">
        <span className="contacts-avatar" aria-hidden="true">
          {initials(row)}
        </span>
        <div className="contacts-card-who">
          <span
            className={row.display_name === null ? "contacts-card-name contacts-missing" : "contacts-card-name"}
            title={row.display_name ?? undefined}
          >
            {row.display_name ?? "no name recorded"}
          </span>
          <span className="contacts-card-address">{row.address}</span>
        </div>
      </div>
      {/* §6.13's badges, every one derived from this one row — there is no
          daemon-side rollup to read any of them off instead. */}
      {(row.verdict !== null || row.outbound_ever === 1 || merged) && (
        <div className="contacts-card-badges">
          {row.verdict === "pin" && <Badge tone="pending">urgent</Badge>}
          {row.verdict === "mute" && <Badge tone="off">noise</Badge>}
          {row.outbound_ever === 1 && <Badge tone="info">you write back</Badge>}
          {merged && <Badge tone="shadow">merged</Badge>}
        </div>
      )}
      {/* The unmerge stays out of the action row: an extra control there wrapped
          to a second line and lifted that card's Pin above every other card's
          (owner's call, 2026-10-09). It sits on the line that states the link it
          undoes — beside the badges it wrapped onto a line of its own, apart from
          the sentence it answers. */}
      {(merged || row.linked_by === "human") && (
        <div className="contacts-sameas">
          {merged && <span>Same person as {sameAs.join(", ")}</span>}
          <UnmergeButton address={row.address} linkedBy={row.linked_by} onProblem={onProblem} />
        </div>
      )}
      <p className="contacts-meta">
        {plural(row.messages_in, "message")} in · last seen{" "}
        <RelativeTime at={row.last_seen} />
      </p>
      <div className="contacts-card-actions">
        <SenderVerdictToggles
          address={row.address}
          verdict={row.verdict}
          name={row.display_name ?? row.address}
          onProblem={onProblem}
          onVerdictSet={onVerdictSet}
        />
      </div>
      {problem !== null && <div className="contacts-problem">{problem}</div>}
    </li>
  );
}

/* -------------------------------------------------------- sender verdict -- */

function SenderVerdictToggles({
  address,
  verdict,
  name,
  onProblem,
  onVerdictSet,
}: {
  address: string;
  verdict: string | null;
  name: string;
  onProblem: ReportProblem;
  onVerdictSet: (options: UndoToastOptions) => void;
}) {
  const mutate = useSenderVerdict();
  const send = (next: "pin" | "mute" | null, isUndo = false) =>
    mutate.mutate(
      { address, verdict: next },
      {
        onSuccess: () => {
          onProblem(null);
          if (isUndo) return;
          // Undo restores what was there before this press, through the same door.
          const before = verdict === "pin" || verdict === "mute" ? verdict : null;
          const said =
            next === "pin" ? `${name} marked urgent` : next === "mute" ? `${name} marked noise` : `${name} verdict cleared`;
          onVerdictSet({ message: said, onUndo: () => send(before, true) });
        },
        onError: (error) => onProblem(<VerdictError error={error} />),
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
  return <ErrorNote>The {DAEMON_NAME} is not responding: that verdict was not recorded</ErrorNote>;
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
            onSuccess: () => onProblem(null),
            onError: (error) => onProblem(<UnmergeError error={error} />),
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
  return <ErrorNote>The {DAEMON_NAME} is not responding: that address was not pulled apart</ErrorNote>;
}
