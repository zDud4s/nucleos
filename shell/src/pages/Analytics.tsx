import { useNavigate, useSearch } from "@tanstack/react-router";
import { AnalyticsView } from "../analytics/AnalyticsView";
import { DEFAULT_DAYS, validateAnalyticsSearch } from "../data/analytics";
import { PageHeader } from "../ui";

/**
 * Trends across every project (spec analytics-fundacao §9). The window is in the address
 * (`?days=`), so a view somebody is reading survives a reload and can be linked; a project's own
 * trends are its Workspace's Analytics mode, which keeps its window in local state.
 */
export function Analytics() {
  const { days } = validateAnalyticsSearch(useSearch({ strict: false }) as Record<string, unknown>);
  const navigate = useNavigate();
  return (
    <>
      <PageHeader title="Analytics" />
      <AnalyticsView
        days={days ?? DEFAULT_DAYS}
        onDaysChange={(next) => void navigate({ to: "/analytics", search: { days: next }, replace: true })}
      />
    </>
  );
}
