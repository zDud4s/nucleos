// §spec correspondent-contacts
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
  Quiet,
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
  const merged = mergedContactIds(rows).size;
  const noun = rows.length === 1 ? "address" : "addresses";
  const first = `${rows.length} ${noun}`;
  return merged === 0 ? first : `${first}, ${merged} sharing an identity with another`;
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
          <Quiet says="no one has written in yet">
            <p>Every address that has sent or received mail lands here, one row per address.</p>
          </Quiet>
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
        <Quiet says="nothing looks like the same person">
          <p>
            The heuristic sweeps the roster on its own; an empty list is it finding no match, not a
            sign anything here is stuck.
          </p>
        </Quiet>
      </Section>
    );
  }

  return (
    <Panel title="Identity questions" aside={<Count n={view.data?.length} />}>
      <p className="contacts-note">
        Two records that look like one person. Saying yes moves a pointer, undoable one address at a
        time from the roster below. Saying no is recorded too — permanently: the pair is refused in
        the same transaction as the answer, so this exact question is never asked again.
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
  return (
    <Row className="contacts-question">
      <div className="contacts-question-head">
        <span className="contacts-question-id">question #{suggestion.proposal_id}</span>
        <RelativeTime at={suggestion.created_at} />
      </div>
      <p className="contacts-reasoning">{suggestion.reasoning}</p>
      <div className="contacts-merge">
        <MergeSideView side={suggestion.keep} role="kept" />
        <MergeSideView side={suggestion.absorb} role="absorbed" />
      </div>
      {mergeVerdictsConflict(suggestion) && (
        <ConflictNote>
          These two carry standing decisions that disagree, so approving will be refused — settle one
          of them and decide this again.
        </ConflictNote>
      )}
      <p className="contacts-permanent">Refusing below is permanent — this exact pair is never suggested again.</p>
      <div className="contacts-actions">
        <ConfirmButton
          label="Yes, one person"
          confirmLabel="They are one person"
          variant="approve"
          disabled={decide.isPending}
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
        <p className="contacts-meta">{side.messages_in} messages in</p>
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
          {outcome.merged === true ? "the two are one person now" : "recorded"}
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

function PeopleRoster({ rows }: { rows: Correspondent[] }) {
  const merged = mergedContactIds(rows);
  // Hairline-ruled and not a column of cards: this is read by scanning down it,
  // not by picking rows out of it — the same posture the mail queue takes, and
  // the argument `.ui-rows` now carries for all four lists that had it.
  return (
    <Rows label="People">
      {rows.map((row) => (
        <PersonRow key={row.address} row={row} merged={merged.has(row.contact_id)} />
      ))}
    </Rows>
  );
}

function PersonRow({ row, merged }: { row: Correspondent; merged: boolean }) {
  return (
    <Row>
      <div className="contacts-row-head">
        <span className="contacts-row-address">{row.address}</span>
        <span className="contacts-row-name">{row.display_name ?? "no name recorded"}</span>
        {/* §6.13's badges, every one derived from this one row — there is no
            daemon-side rollup to read any of them off instead. */}
        {row.verdict === "pin" && <Badge tone="pending">urgent</Badge>}
        {row.verdict === "mute" && <Badge tone="off">noise</Badge>}
        {row.outbound_ever === 1 && <Badge tone="info">you write back</Badge>}
        {merged && <Badge tone="shadow">merged</Badge>}
        <RelativeTime at={row.last_seen} />
      </div>
      <p className="contacts-meta">
        {row.messages_in} messages in — first seen <RelativeTime at={row.first_seen} />
      </p>
      <div className="contacts-row-actions">
        <SenderVerdictToggles address={row.address} verdict={row.verdict} />
        <UnmergeButton address={row.address} linkedBy={row.linked_by} />
      </div>
    </Row>
  );
}

/* -------------------------------------------------------- sender verdict -- */

function SenderVerdictToggles({ address, verdict }: { address: string; verdict: string | null }) {
  const mutate = useSenderVerdict();
  return (
    <div className="contacts-actions">
      <Button
        variant={verdict === "pin" ? "approve" : "ghost"}
        disabled={mutate.isPending}
        onClick={() => mutate.mutate({ address, verdict: "pin" })}
      >
        Pin
      </Button>
      <Button disabled={mutate.isPending} onClick={() => mutate.mutate({ address, verdict: "mute" })}>
        Mute
      </Button>
      <Button
        disabled={mutate.isPending || verdict === null}
        onClick={() => mutate.mutate({ address, verdict: null })}
      >
        Clear
      </Button>
      {mutate.isError && <VerdictError error={mutate.error} />}
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
function UnmergeButton({ address, linkedBy }: { address: string; linkedBy: string }) {
  const unmerge = useUnmerge();
  if (linkedBy !== "human") return null;

  return (
    <div className="contacts-actions">
      <ConfirmButton
        label="Not the same person"
        confirmLabel="Pull this address back apart"
        variant="quiet"
        disabled={unmerge.isPending}
        onConfirm={() => unmerge.mutate(address)}
      />
      {unmerge.isError && <UnmergeError error={unmerge.error} />}
    </div>
  );
}

function UnmergeError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return <RefusalNote refusal={error} sentences={{ not_found: "that address is no longer in the roster" }} />;
  }
  return <ErrorNote>the núcleo did not answer — that address was not pulled apart</ErrorNote>;
}
