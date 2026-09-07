import { useEffect, useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  NOTIFY_FAMILIES,
  groupKinds,
  readFeedKind,
  type FamilyRow,
  type KindRow,
  type KindVerdict,
  type NotifyPolicy,
  type NotifyRule,
} from "../data/feed";
import { useNotifyPolicy, useObservedKinds, useSetNotifyPolicy } from "../data/system";
import { Button, ErrorNote, Panel, RefusalNote } from "../ui";

/**
 * Which notifications reach Telegram.
 *
 * The screen is the resolution of the policy, drawn — not a second idea laid
 * over it. `groupKinds` resolves once, in `data/feed.ts`, and everything here
 * renders what it returned. A component that re-read the policy to work out a
 * switch's state would be a second implementation of the same rules, free to
 * disagree with the one the sidecar actually obeys.
 *
 * The edits live in local state until the save button sends the whole policy,
 * because the núcleo replaces rather than merges (a partial write would let the
 * two halves drift) and because a switch that fired a request per flip would
 * make a settings screen into a stream of writes.
 */
export function NotificationsView() {
  const stored = useNotifyPolicy();
  const observed = useObservedKinds();
  const save = useSetNotifyPolicy();

  // The draft. Seeded from the núcleo and re-seeded whenever the stored policy
  // arrives again — which is a save settling, not a clock, because neither read
  // polls. `stored.data` is a new object per fetch, so the effect keys on the
  // fetch rather than on a deep compare nobody would maintain.
  const [draft, setDraft] = useState<NotifyPolicy | null>(null);
  useEffect(() => {
    if (stored.data) setDraft(stored.data);
  }, [stored.data]);

  if (stored.isError) return <NotificationsError error={stored.error} what="the notification policy" />;
  if (observed.isError) return <NotificationsError error={observed.error} what="the kinds this machine writes" />;
  if (!draft || !observed.data) return <p className="sy-note">reading the notification policy…</p>;

  const { families, loose } = groupKinds(observed.data, draft);
  const dirty = !samePolicy(draft, stored.data);

  function setFamily(selector: string, on: boolean) {
    // Read off the NÚCLEO's policy, not the draft. By the time somebody flips a
    // switch back the draft already holds the `false` they just wrote, so asking
    // the draft what was there originally always answers `false` — and the
    // explicit `true` this exists to preserve is gone by then.
    const asStored = stored.data?.families.find((f) => f.selector === selector);

    setDraft((current) => {
      if (!current) return current;
      const families = current.families.filter((f) => f.selector !== selector);

      // A family switch writes EXACTLY ONE rule, which is what one-family-one-
      // prefix buys. Turning one OFF always writes `false`.
      //
      // Turning one back ON depends on what the núcleo holds. If the owner had
      // written an explicit `enabled: true` — "the jobs stay on", inert but
      // deliberate — it is KEPT, per spec §5.3: deleting it would lose what they
      // put there. If there was no rule, or the stored rule was the `false` being
      // undone, we go back to no rule at all, because absence is how a machine
      // nobody has configured behaves and re-storing `true` would claim otherwise.
      if (!on) families.push({ selector, enabled: false });
      else if (asStored?.enabled === true) families.push(asStored);

      return { ...current, families };
    });
  }

  function setKind(kind: string, verdict: KindVerdict) {
    setDraft((current) => {
      if (!current) return current;
      const kinds = current.kinds.filter((k) => k.selector !== kind);
      if (verdict !== "inherit") kinds.push({ selector: kind, enabled: verdict === "always" });
      return { ...current, kinds };
    });
  }

  return (
    <Panel title="Telegram notifications">
      <p className="sy-note">
        A family is a prefix, and a kind can override its family. Anything no rule claims is sent —
        so a machine nobody has configured behaves exactly as it did before this screen existed.
      </p>
      <p className="sy-note">
        Proposals, the kill switch and budget alerts are never silenced by these switches, whatever
        you set here. The sidecar re-reads this on its next round: nothing needs restarting.
      </p>

      {save.isError &&
        (isApiRefusal(save.error) ? (
          <RefusalNote refusal={save.error} />
        ) : (
          <ErrorNote>the núcleo did not answer — nothing was saved</ErrorNote>
        ))}

      <ul className="sy-notify-families">
        {families.map((family) => (
          <FamilyItem key={family.selector} family={family} onFamily={setFamily} onKind={setKind} />
        ))}
      </ul>

      {loose.length > 0 && (
        <>
          <h3 className="sy-notify-heading">On their own</h3>
          <p className="sy-note">
            Kinds no family prefix claims. Each is silenced on its own; there is no group switch
            because there is no group — the grouping here is presentation, not a rule.
          </p>
          <ul className="sy-notify-kinds">
            {loose.map((row) => (
              <KindItem key={row.kind} row={row} onKind={setKind} inFamily={false} />
            ))}
          </ul>
        </>
      )}

      <div className="sy-notify-actions">
        <Button onClick={() => save.mutate(draft)} disabled={!dirty || save.isPending}>
          {save.isPending ? "saving…" : "Save"}
        </Button>
        {dirty && <span className="sy-note">unsaved changes</span>}
      </div>
    </Panel>
  );
}

/**
 * Whether two policies say the same thing — by CONTENT, not by array order.
 *
 * Editing a switch rewrites its rule by removing and re-appending it, so
 * turning a family off and straight back on leaves a policy that is identical
 * in meaning and different in order. A `JSON.stringify` comparison would call
 * that unsaved work and leave the button lit over nothing to save.
 */
function samePolicy(a: NotifyPolicy, b: NotifyPolicy | undefined): boolean {
  if (!b) return false;
  const normalise = (rules: NotifyRule[]) =>
    [...rules]
      .map((rule) => `${rule.selector}=${rule.enabled}`)
      .sort()
      .join("|");
  return (
    normalise(a.families) === normalise(b.families) && normalise(a.kinds) === normalise(b.kinds)
  );
}

function NotificationsError({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about {what}</ErrorNote>;
}

function FamilyItem({
  family,
  onFamily,
  onKind,
}: {
  family: FamilyRow;
  onFamily: (selector: string, on: boolean) => void;
  onKind: (kind: string, verdict: KindVerdict) => void;
}) {
  const [open, setOpen] = useState(false);
  // `null` — no stored rule — draws as ON, because that is what it does.
  const on = family.rule ?? true;
  const known = NOTIFY_FAMILIES.some((f) => f.selector === family.selector);

  return (
    <li className="sy-notify-family">
      <div className="sy-notify-family-row">
        <label className="sy-notify-switch">
          <input
            type="checkbox"
            checked={on}
            onChange={(event) => onFamily(family.selector, event.target.checked)}
          />
          <span className="sy-notify-label">{family.label ?? family.selector}</span>
        </label>
        {/* A prefix somebody stored that this build has no name for. Shown as itself rather than
            hidden: the rule is in force either way, and a rule you cannot see is one you cannot
            undo. */}
        {!known && <span className="sy-note">unknown family</span>}
        <button type="button" className="sy-notify-disclose" onClick={() => setOpen(!open)}>
          {family.kinds.length} kind{family.kinds.length === 1 ? "" : "s"} {open ? "▾" : "▸"}
        </button>
      </div>
      {open && family.kinds.length > 0 && (
        <ul className="sy-notify-kinds">
          {family.kinds.map((row) => (
            <KindItem key={row.kind} row={row} onKind={onKind} />
          ))}
        </ul>
      )}
      {open && family.kinds.length === 0 && (
        <p className="sy-note">
          This machine has written none of these in the last 90 days. The switch still covers the
          ones it writes next.
        </p>
      )}
    </li>
  );
}

function KindItem({
  row,
  onKind,
  inFamily = true,
}: {
  row: KindRow;
  onKind: (kind: string, verdict: KindVerdict) => void;
  /** `false` for a kind no family prefix claims — it has nothing to inherit from. */
  inFamily?: boolean;
}) {
  // The only thing FEED_KINDS is used for on this screen, and the one place its
  // gaps are harmless: `readFeedKind` already falls back to the literal.
  const label = readFeedKind(row.kind)?.label ?? row.kind;

  return (
    <li className="sy-notify-kind">
      <span className="sy-notify-kind-name" title={row.kind}>
        {label}
      </span>
      {!row.recentlySeen && (
        // Kept on screen rather than hidden. It is a rule somebody wrote; hiding
        // it would leave it silencing with nowhere to undo it.
        <span className="sy-note">not seen in the last 90 days</span>
      )}
      {inFamily ? (
        <select
          value={row.verdict}
          aria-label={row.kind}
          onChange={(event) => onKind(row.kind, event.target.value as KindVerdict)}
        >
          <option value="inherit">follows its family</option>
          <option value="always">always</option>
          <option value="never">never</option>
        </select>
      ) : (
        // A loose kind has no family to follow, so offering "follows its family"
        // would name a thing that does not exist. Spec §7.3 asks for a plain
        // switch here; `inherit` and `always` both send it, so the two states
        // this draws are the two it can be in.
        <input
          type="checkbox"
          aria-label={row.kind}
          checked={row.verdict !== "never"}
          onChange={(event) => onKind(row.kind, event.target.checked ? "inherit" : "never")}
        />
      )}
    </li>
  );
}
