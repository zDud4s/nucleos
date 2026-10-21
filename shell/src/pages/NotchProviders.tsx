import { Panel, Quiet, Skeleton, Switch } from "../ui";
import { useHiddenProviders } from "../app/notch-providers";
import { useQuota } from "../data/quota";

/**
 * Which subscriptions the quota notch draws, one switch each, beside the quota brake.
 *
 * The list is what the sidecar reports plus whatever is hidden, so a provider hidden once can be
 * brought back even on a day the sidecar did not report it. A subscription never signed in on this
 * machine is not in it at all: the sidecar leaves those out before they reach here.
 *
 * Saved on the click, with no confirm: it is a preference of this screen, nothing goes to the
 * daemon, and undoing it is the same click.
 */
export function NotchProviders() {
  const quota = useQuota();
  const [hidden, setShown] = useHiddenProviders();
  const reported = (quota.data?.providers ?? []).map((item) => item.provider);
  const names = [...reported, ...hidden.filter((name) => !reported.includes(name))];

  return (
    <Panel title="Quota notch">
      <p className="sy-note sy-notch-note">
        A preference of this screen only: it is kept in this browser, not on the background service,
        and does not follow you to another machine. Each switch below decides whether the notch at
        the top of the window draws that subscription.
      </p>
      {quota.isPending && names.length === 0 ? (
        <Skeleton label="Reading the subscriptions" />
      ) : names.length === 0 ? (
        <Quiet compact says="No subscription has been read on this machine yet. Sign in to one and it will appear here." />
      ) : (
        <ul className="sy-notch-list" aria-label="Subscriptions">
          {names.map((name) => (
            <li key={name}>
              <Switch label={`Show ${name}`} checked={!hidden.includes(name)} onChange={(next) => setShown(name, next)} />
            </li>
          ))}
        </ul>
      )}
      <p className="sy-note sy-notch-note">
        Saved as you switch. Only what the notch draws changes; the quota brake is not affected.
      </p>
    </Panel>
  );
}
