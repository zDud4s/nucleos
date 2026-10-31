import { DAEMON_NAME } from "../copy";
import { ApiRefusal } from "../data/client";
import { useSettingHistory } from "../data/settings";
import { Button, ErrorNote, RefusalNote, RelativeTime, Skeleton } from "../ui";

export interface SettingHistoryProps {
  scope: string;
  name: string;
  /** The version the form is showing: its row is marked and cannot be restored. */
  current: number;
  /** Why no version can be restored right now, or `null` when one can. */
  blockedBecause: string | null;
  onRestore: (target: number) => void;
}

/**
 * Every version of one document, newest first.
 *
 * It lists and nothing else. The restore itself belongs to `SettingsForm`, which owns the notice and the
 * editor a restore has to move. Sorted here even though the núcleo answers newest first, because a list
 * that reads upside down because a route changed its ordering would be a silent lie about what "current" is.
 */
export function SettingHistory({ scope, name, current, blockedBecause, onRestore }: SettingHistoryProps) {
  const history = useSettingHistory(scope, name, true);

  if (history.data === undefined) {
    if (history.isPending) return <Skeleton label={`Reading the versions of ${name}`} lines={3} />;
    if (history.error instanceof ApiRefusal) return <RefusalNote refusal={history.error} />;
    return (
      <ErrorNote onRetry={() => void history.refetch()}>
        the {DAEMON_NAME} is not responding: the versions of this document are not known
      </ErrorNote>
    );
  }

  if (history.data.length === 0) {
    return <p className="sy-meta">No versions yet: this document was never written.</p>;
  }

  const rows = [...history.data].sort((a, b) => b.version - a.version);

  return (
    <ul aria-label={`Versions of ${name}`} className="flex flex-col gap-2">
      {rows.map((row) => (
        <li key={row.version} className="flex flex-wrap items-center gap-2">
          <span className="sy-meta">version {row.version}</span>
          <span className="sy-meta">
            <RelativeTime at={row.written_at} />
          </span>
          <span className="sy-meta">{row.written_by}</span>
          {row.deleted && <span className="sy-meta">deleted</span>}
          {row.version === current ? (
            <span className="sy-meta">current</span>
          ) : row.deleted ? null : (
            <Button
              aria-label={`Restore version ${row.version}`}
              disabled={blockedBecause !== null}
              title={blockedBecause ?? undefined}
              onClick={() => onRestore(row.version)}
            >
              Restore
            </Button>
          )}
        </li>
      ))}
    </ul>
  );
}
