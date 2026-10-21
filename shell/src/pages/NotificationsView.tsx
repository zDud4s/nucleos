import { useEffect, useId, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { isApiRefusal } from "../data/client";
import {
  NOTIFY_FAMILIES,
  groupKinds,
  mergePolicy,
  ruleSource,
  readFeedKind,
  type FamilyRow,
  type KindRow,
  type KindVerdict,
  type NotifyPolicy,
  type NotifyRule,
} from "../data/feed";
import { useChannels } from "../data/channels";
import { useDesktopNotify } from "../data/desktop-notify-pref";
import {
  useChannelNotifyPolicy,
  useNotifyPolicy,
  useObservedKinds,
  useSetChannelNotifyPolicy,
  useSetNotifyPolicy,
} from "../data/system";
import { Button, Callout, ErrorNote, Panel, Quiet, RefusalNote, Section, Skeleton, Switch } from "../ui";
import { DAEMON_NAME } from "../copy";
import { plural } from "../lib/plural";

/**
 * Which notifications reach the messaging channels.
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
export function NotificationsView({
  channel,
  onChannelChange,
}: {
  channel?: string;
  onChannelChange?: (channel: string | undefined) => void;
} = {}) {
  // Two modes, two components: each owns its own queries, so no hook is ever called conditionally.
  // The channel is the key, so a switch of channel starts a fresh draft instead of carrying edits
  // from one channel's overrides into another's.
  return channel === undefined ? (
    <DefaultMode onChannelChange={onChannelChange} />
  ) : (
    <ChannelMode key={channel} channel={channel} onChannelChange={onChannelChange} />
  );
}

/**
 * A draft seeded from the núcleo and re-seeded whenever the stored policy arrives again — a save
 * settling, not a clock, because neither read polls. A refetch that brings the SAME content
 * (window focus, say) must not wipe edits made meanwhile; only a policy that differs re-seeds.
 */
function useDraft(stored: NotifyPolicy | undefined) {
  const [draft, setDraft] = useState<NotifyPolicy | null>(null);
  const seeded = useRef<NotifyPolicy | null>(null);
  useEffect(() => {
    if (!stored) return;
    if (seeded.current !== null && samePolicy(seeded.current, stored)) return;
    seeded.current = stored;
    setDraft(stored);
  }, [stored]);
  return [draft, setDraft] as const;
}

/** The scope every channel shares: the original screen, unchanged. */
function DefaultMode({
  onChannelChange,
}: {
  onChannelChange?: (channel: string | undefined) => void;
}) {
  const stored = useNotifyPolicy();
  const observed = useObservedKinds();
  const save = useSetNotifyPolicy();
  const [draft, setDraft] = useDraft(stored.data);

  if (stored.isError) return <NotificationsError error={stored.error} what="the notification policy" />;
  if (observed.isError) return <NotificationsError error={observed.error} what="the kinds this machine writes" />;
  if (!draft || !stored.data || !observed.data) {
    return <Skeleton label="Reading the notification policy" lines={3} />;
  }

  return (
    <PolicyEditor
      stored={stored.data}
      draft={draft}
      setDraft={setDraft}
      observed={observed.data}
      save={save}
      onChannelChange={onChannelChange}
    />
  );
}

/**
 * One channel's overrides, drawn over the default policy.
 *
 * The draft is the OVERRIDES only; what is drawn is `groupKinds` over the default with the draft
 * laid on top, so the resolution stays in one place. Save sends the overrides and nothing else —
 * never the default or the resolved view, which the núcleo owns.
 */
function ChannelMode({
  channel,
  onChannelChange,
}: {
  channel: string;
  onChannelChange?: (channel: string | undefined) => void;
}) {
  const stored = useChannelNotifyPolicy(channel);
  const observed = useObservedKinds();
  const save = useSetChannelNotifyPolicy(channel);
  const [draft, setDraft] = useDraft(stored.data?.overrides);

  if (stored.isError) return <NotificationsError error={stored.error} what="the notification policy" />;
  if (observed.isError) return <NotificationsError error={observed.error} what="the kinds this machine writes" />;
  if (!draft || !stored.data || !observed.data) {
    return <Skeleton label="Reading the notification policy" lines={3} />;
  }

  return (
    <PolicyEditor
      stored={stored.data.overrides}
      base={stored.data.default}
      primary={stored.data.primary}
      draft={draft}
      setDraft={setDraft}
      observed={observed.data}
      save={save}
      channel={channel}
      onChannelChange={onChannelChange}
    />
  );
}

interface SaveMutation {
  mutate(policy: NotifyPolicy, options?: { onSuccess?: () => void }): void;
  isPending: boolean;
  isError: boolean;
  error: unknown;
}

/**
 * The switches, shared by both modes. `base` is the default policy when `channel` is set: then the
 * draft holds overrides, each row carries its source, and a row with an override can be reset.
 */
function PolicyEditor({
  stored,
  base,
  primary,
  draft,
  setDraft,
  observed,
  save,
  channel,
  onChannelChange,
}: {
  stored: NotifyPolicy;
  base?: NotifyPolicy;
  primary?: boolean;
  draft: NotifyPolicy;
  setDraft: Dispatch<SetStateAction<NotifyPolicy | null>>;
  observed: string[];
  save: SaveMutation;
  channel?: string;
  onChannelChange?: (channel: string | undefined) => void;
}) {
  const [saved, setSaved] = useState(false);

  // In channel mode the rows show the overrides laid over the default.
  const shown = base ? mergePolicy(base, draft) : draft;
  const { families, loose } = groupKinds(observed, shown);
  const dirty = !samePolicy(draft, stored);
  const sourceOf = (selector: string, scope: "families" | "kinds") =>
    base ? ruleSource(selector, scope, base, draft) : undefined;

  function setFamily(selector: string, on: boolean) {
    setSaved(false);
    // Read off the stored policy, not the draft. By the time somebody flips a
    // switch back the draft already holds the `false` they just wrote, so asking
    // the draft what was there originally always answers `false` — and the
    // explicit `true` this exists to preserve is gone by then.
    const asStored = stored.families.find((f) => f.selector === selector);
    const baseRule = base?.families.find((f) => f.selector === selector);

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
      //
      // On a channel, absence means "follow the default", so when the default itself says
      // `false` an explicit `true` override is the only way to turn the family on.
      if (!on) families.push({ selector, enabled: false });
      else if (asStored?.enabled === true) families.push(asStored);
      else if (baseRule?.enabled === false) families.push({ selector, enabled: true });

      return { ...current, families };
    });
  }

  function setKind(kind: string, verdict: KindVerdict) {
    setSaved(false);
    setDraft((current) => {
      if (!current) return current;
      const kinds = current.kinds.filter((k) => k.selector !== kind);
      if (verdict !== "inherit") kinds.push({ selector: kind, enabled: verdict === "always" });
      return { ...current, kinds };
    });
  }

  // A loose kind has no family to follow: its checkbox sends `inherit` for "on". Where the default
  // silences it, "inherit" would leave it silenced, so the channel needs an explicit `always`.
  function setLooseKind(kind: string, verdict: KindVerdict) {
    const silencedByDefault = base?.kinds.some((k) => k.selector === kind && !k.enabled) ?? false;
    setKind(kind, verdict === "inherit" && silencedByDefault ? "always" : verdict);
  }

  /** Drop the channel's own rule, so the row follows the default again. */
  function reset(selector: string, scope: "families" | "kinds") {
    setSaved(false);
    setDraft((current) =>
      current ? { ...current, [scope]: current[scope].filter((r) => r.selector !== selector) } : current,
    );
  }

  const empty = draft.families.length === 0 && draft.kinds.length === 0;

  return (
    <>
      <DesktopSwitch />
      <Panel
        title="Notifications"
        /* Save sits in the head, where it is on screen whatever is expanded below it. */
        aside={
          <div className="sy-notify-actions">
            {/* Drawn only when the parent can act on a choice; `System` passes it. */}
            {onChannelChange && (
              <ChannelScopeSelector channel={channel} onChannelChange={onChannelChange} />
            )}
            {dirty && <span className="sy-meta">unsaved changes</span>}
            <span className="sy-meta" role="status">
              {saved && !dirty ? "saved" : null}
            </span>
            <Button
              variant="primary"
              onClick={() => {
                save.mutate(draft, { onSuccess: () => setSaved(true) });
              }}
              disabled={!dirty || save.isPending}
            >
              {save.isPending ? "Saving…" : "Save"}
            </Button>
          </div>
        }
      >
        <div className="sy-notify-never">
          <Callout tone="info">
            Proposals, the kill switch and budget alerts are never silenced by these switches,
            whatever you set here.
          </Callout>
        </div>
        <p className="sy-note">
          A family is a prefix, and a kind can override its family. Anything no rule claims is sent,
          so a machine nobody has configured behaves exactly as it did before this screen existed.
          The adapter picks this up on its next round: nothing needs restarting.
        </p>
        {base && primary === false && (
          <div className="sy-notify-never">
            <Callout tone="pending">
              This channel is dormant: it is not the primary. Its overrides are stored and take effect
              when it becomes the primary.
            </Callout>
          </div>
        )}
        {base && empty && (
          <div className="sy-notify-never">
            <Quiet compact says="This channel inherits everything from the default." />
          </div>
        )}

        {save.isError &&
          (isApiRefusal(save.error) ? (
            <RefusalNote refusal={save.error} />
          ) : (
            <ErrorNote>the {DAEMON_NAME} is not responding: nothing was saved</ErrorNote>
          ))}

        <ul className="sy-notify-families">
          {families.map((family) => (
            <FamilyItem
              key={family.selector}
              family={family}
              onFamily={setFamily}
              onKind={setKind}
              source={sourceOf(family.selector, "families")}
              sourceOfKind={(kind) => sourceOf(kind, "kinds")}
              onReset={base ? reset : undefined}
            />
          ))}
        </ul>

        {loose.length > 0 && (
          <Section label="On their own" level={3}>
            <p className="sy-note">
              Kinds no family prefix claims. Each is silenced on its own; there is no group switch
              because there is no group: the grouping here is presentation, not a rule.
            </p>
            <ul className="sy-notify-kinds sy-notify-loose">
              {loose.map((row) => (
                <KindItem
                  key={row.kind}
                  row={row}
                  onKind={setLooseKind}
                  inFamily={false}
                  source={sourceOf(row.kind, "kinds")}
                  onReset={base ? reset : undefined}
                />
              ))}
            </ul>
          </Section>
        )}
      </Panel>
    </>
  );
}

/**
 * Picks which scope the policy below applies to: the default for every channel, or one channel.
 *
 * Only configured channels are offered; an unconfigured one has no adapter to read an override. A
 * failed channel list must not take the Default mode down with it, so the error is a quiet note and
 * the selector keeps its one safe option.
 */
function ChannelScopeSelector({
  channel,
  onChannelChange,
}: {
  channel?: string;
  onChannelChange: (channel: string | undefined) => void;
}) {
  const channels = useChannels();
  const selectId = useId();
  const options = channels.data?.channels.filter((c) => c.configured) ?? [];
  const primary = channels.data?.primary ?? null;

  return (
    <>
      <label className="sy-notify-scope-label" htmlFor={selectId}>
        Channel
      </label>
      <select
        id={selectId}
        className="sy-notify-select"
        value={channel ?? ""}
        disabled={channels.isPending}
        onChange={(event) => onChannelChange(event.target.value === "" ? undefined : event.target.value)}
      >
        <option value="">Default (all channels)</option>
        {options.map((c) => (
          <option key={c.id} value={c.id}>
            {c.id === primary ? `${c.label} (primary)` : c.label}
          </option>
        ))}
      </select>
      {channels.isError && <span className="sy-notify-scope-label">the channel list did not load</span>}
    </>
  );
}

/**
 * The desktop channel's switch: this computer's own toasts, next to Telegram's.
 *
 * Not part of the draft below it. It is a preference of this window, saved the moment it is
 * flipped, and the policy the Telegram switches write is the one the toasts obey too — so there is
 * nothing here to send to the núcleo.
 */
function DesktopSwitch() {
  const [on, setOn] = useDesktopNotify();
  return (
    <Panel title="Desktop notifications">
      <Switch
        label="Also show these on this computer's desktop when the window is not in front"
        checked={on}
        onChange={setOn}
      />
      <p className="sy-note sy-notify-lead">
        Waiting approvals, conversations that answered, and the feed lines the choices below allow
        (the same ones your channels get).
      </p>
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
  return <ErrorNote>the {DAEMON_NAME} is not responding: nothing is known about {what}</ErrorNote>;
}

type RuleSource = "default" | "override" | "pinned";

/** The origin mark of a rule on a channel, with "Reset to default" when the channel has its own. */
function SourceMark({
  source,
  onReset,
  what,
}: {
  source: RuleSource;
  onReset?: () => void;
  what: string;
}) {
  return (
    <>
      <span
        className={`sy-rule-source sy-rule-source-${source}`}
        title={source === "pinned" ? "pinned to the default" : undefined}
      >
        {source}
      </span>
      {source !== "default" && onReset && (
        <Button variant="quiet" aria-label={`Reset to default: ${what}`} onClick={onReset}>
          Reset to default
        </Button>
      )}
    </>
  );
}

function FamilyItem({
  family,
  onFamily,
  onKind,
  source,
  sourceOfKind,
  onReset,
}: {
  family: FamilyRow;
  onFamily: (selector: string, on: boolean) => void;
  onKind: (kind: string, verdict: KindVerdict) => void;
  /** Channel mode only: where this family's value comes from. */
  source?: RuleSource;
  sourceOfKind?: (kind: string) => RuleSource | undefined;
  onReset?: (selector: string, scope: "families" | "kinds") => void;
}) {
  const [open, setOpen] = useState(false);
  const card = useRef<HTMLLIElement>(null);
  const popId = useId();

  /*
    The kinds open OVER the page, not inside the card: grown in place, one open family pushed
    every card below it down by its whole list. So it behaves like a menu — a click anywhere
    outside the card, or Escape, closes it.
  */
  useEffect(() => {
    if (!open) return undefined;
    function onPointer(event: PointerEvent) {
      if (card.current !== null && !card.current.contains(event.target as Node)) setOpen(false);
    }
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setOpen(false);
        // The kinds unmount with the popover; without this the focus falls to the body.
        card.current?.querySelector<HTMLButtonElement>("button[aria-expanded]")?.focus();
      }
    }
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // `null` — no stored rule — draws as ON, because that is what it does.
  const on = family.rule ?? true;
  const known = NOTIFY_FAMILIES.some((f) => f.selector === family.selector);

  return (
    <li ref={card} className={open ? "sy-notify-family sy-notify-family-open" : "sy-notify-family"}>
      <div className="sy-notify-family-row">
        <Switch
          label={family.label ?? family.selector}
          checked={on}
          onChange={(next) => onFamily(family.selector, next)}
        />
        {/* A prefix somebody stored that this build has no name for. Shown as itself rather than
            hidden: the rule is in force either way, and a rule you cannot see is one you cannot
            undo. */}
        {!known && <span className="sy-note">unknown family</span>}
        {source && (
          <SourceMark
            source={source}
            what={family.label ?? family.selector}
            onReset={onReset && (() => onReset(family.selector, "families"))}
          />
        )}
        <span className="sy-notify-disclose">
          <Button
            variant="quiet"
            aria-expanded={open}
            aria-controls={open ? popId : undefined}
            onClick={() => setOpen(!open)}
          >
            {plural(family.kinds.length, "kind")} <span aria-hidden="true">{open ? "▾" : "▸"}</span>
          </Button>
        </span>
      </div>
      {open && family.kinds.length > 0 && (
        <ul id={popId} className="sy-notify-kinds sy-notify-pop">
          {family.kinds.map((row) => (
            <KindItem
              key={row.kind}
              row={row}
              onKind={onKind}
              source={sourceOfKind?.(row.kind)}
              onReset={onReset}
            />
          ))}
        </ul>
      )}
      {open && family.kinds.length === 0 && (
        <p id={popId} className="sy-note sy-notify-pop">
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
  source,
  onReset,
}: {
  row: KindRow;
  onKind: (kind: string, verdict: KindVerdict) => void;
  /** Channel mode only: where this kind's value comes from. */
  source?: RuleSource;
  onReset?: (selector: string, scope: "families" | "kinds") => void;
  /** `false` for a kind no family prefix claims — it has nothing to inherit from. */
  inFamily?: boolean;
}) {
  // The only thing the state map's `feed` domain is used for on this screen, and
  // the one place its gaps are harmless: `readFeedKind` falls back to the literal.
  const label = readFeedKind(row.kind)?.label ?? row.kind;
  // The visible name IS the control's name (WCAG 2.5.3): a real label, not a second string.
  const controlId = useId();

  return (
    <li className="sy-notify-kind">
      <label className="sy-notify-kind-name" htmlFor={controlId} title={row.kind}>
        {label}
      </label>
      {!row.recentlySeen && (
        // Kept on screen rather than hidden. It is a rule somebody wrote; hiding
        // it would leave it silencing with nowhere to undo it.
        <span className="sy-note">not seen in the last 90 days</span>
      )}
      {source && (
        <SourceMark
          source={source}
          what={label}
          onReset={onReset && (() => onReset(row.kind, "kinds"))}
        />
      )}
      {inFamily ? (
        <select
          className="sy-notify-select"
          id={controlId}
          value={row.verdict}
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
        <Switch
          label={label}
          labelHidden
          id={controlId}
          checked={row.verdict !== "never"}
          onChange={(next) => onKind(row.kind, next ? "inherit" : "never")}
        />
      )}
    </li>
  );
}
