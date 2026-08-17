import { useBudget } from "../data/system";

/**
 * Money, in the corner of the rail.
 *
 * The one number that is on screen on every page, because it is the one that
 * decides whether anything autonomous will run at all. Two dollars and a
 * ceiling is a whole sentence about the state of the machine.
 *
 * `limit_usd === null` is "no ceiling" and never a zero. The distinction is not
 * cosmetic: a ceiling of zero means nothing will ever run, and no ceiling means
 * nothing will ever stop — rendering the first as the second, or either as a
 * bare `0.00`, is the shell asserting a policy nobody set.
 */
export function BudgetLine() {
  const budget = useBudget();
  const view = budget.data;

  if (view === undefined) {
    // Never a zero while the first read is in flight. An unread budget is not a
    // spent budget, and the difference is the whole point of the line.
    return (
      <p className="app-budget app-budget-unread" role="status">
        <span className="app-budget-figure">$ —</span>
        <span className="app-budget-scope">window spend unread</span>
      </p>
    );
  }

  const spend = view.window_spend_usd.toFixed(2);
  const ceiling = view.limit_usd === null ? null : view.limit_usd.toFixed(2);

  return (
    <p className={view.paused ? "app-budget app-budget-paused" : "app-budget"} role="status">
      <span className="app-budget-figure">{ceiling === null ? `$ ${spend}` : `$ ${spend} / ${ceiling}`}</span>
      <span className="app-budget-scope">{ceiling === null ? "window · no ceiling" : "window"}</span>
      {view.paused ? (
        <span className="app-budget-reason">held — {view.reason ?? "a ceiling is holding autonomous work"}</span>
      ) : null}
    </p>
  );
}
