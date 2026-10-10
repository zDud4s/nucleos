import { useHiddenProviders } from "../app/notch-providers";
import { useQuota } from "../data/quota";

/**
 * Which subscriptions the quota notch draws, one checkbox each, beside the quota brake.
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
    <section className="sy-budget-group">
      <h3 className="sy-budget-heading">Quota notch</h3>
      {names.length === 0 ? (
        <p className="sy-field-hint">no subscription has been read on this machine yet</p>
      ) : (
        names.map((name) => (
          <div className="sy-field" key={name}>
            <label className="sy-check">
              <input
                type="checkbox"
                checked={!hidden.includes(name)}
                onChange={(event) => setShown(name, event.target.checked)}
              />
              {`Show ${name}`}
            </label>
          </div>
        ))
      )}
      <p className="sy-field-hint">only what the notch draws; the quota brake is not affected</p>
    </section>
  );
}
