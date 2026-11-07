import { useNavigate, useSearch } from "@tanstack/react-router";
import { AnalyticsView } from "../analytics/AnalyticsView";
import { apiFetch } from "../data/client";
import { keys } from "../data/keys";
import { DEFAULT_DAYS, analyticsPath, validateAnalyticsSearch } from "../data/analytics";
import { registerPrefetcher } from "../lib/prefetch";
import { PageHeader } from "../ui";

// Hovering the rail link warms the default window's report (#7). Same key and path as
// `useAnalytics(undefined, DEFAULT_DAYS)`, so the page paints from the cache on arrival.
registerPrefetcher("/analytics", (qc) =>
  qc.prefetchQuery({
    queryKey: keys.analytics.global(DEFAULT_DAYS),
    queryFn: () => apiFetch(analyticsPath(undefined, DEFAULT_DAYS)),
    staleTime: 30_000,
  }),
);

/**
 * Trends across every project (spec analytics-fundacao §9). The window is in the address
 * (`?days=`, `?section=`), so a view somebody is reading survives a reload and can be linked; a project's own
 * trends are its Workspace's Analytics mode, which keeps its window in local state.
 */
export function Analytics() {
  const { days, section } = validateAnalyticsSearch(useSearch({ strict: false }) as Record<string, unknown>);
  const navigate = useNavigate();
  return (
    <>
      <PageHeader title="Analytics" />
      <AnalyticsView
        days={days ?? DEFAULT_DAYS}
        onDaysChange={(next) =>
          void navigate({
            to: "/analytics",
            search: (prev: Record<string, unknown>) => ({ ...validateAnalyticsSearch(prev), days: next }),
            replace: true,
          })
        }
        section={section ?? "summary"}
        onSectionChange={(next) =>
          void navigate({
            to: "/analytics",
            search: (prev: Record<string, unknown>) => ({ ...validateAnalyticsSearch(prev), section: next }),
            replace: true,
          })
        }
      />
    </>
  );
}
