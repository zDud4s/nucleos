import type { CSSProperties, ReactNode } from "react";
import { Row, Rows, Table, Td, Th } from "../ui";

/*
  Loading shapes for the System tab's lists and fact columns, drawn on the loaded rows' own
  classes (`sy-backup-row`, `sy-token-row`, `sy-config-facts`), so a row has the same grid, pitch
  and insets whether it is a bar or a snapshot. Everything here is `aria-hidden` under a
  `role="status"` wrapper with a visually hidden label, like `ui/Skeleton`.
*/

const bar = (width: string, style?: CSSProperties) => (
  <span className="ui-skeleton-bar" style={{ width, ...style }} />
);

/** A control's height (a ghost or danger `Button`), so a row is as tall as one with its button. */
const BUTTON_HEIGHT = "2rem";

/** One text line's height (a fact's `dd` is `--text-sm`), the bar centred in it. */
const LINE: CSSProperties = { display: "flex", alignItems: "center", minHeight: "1lh", fontSize: "var(--text-sm)" };

function Shell({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div role="status" aria-busy="true">
      <span className="sr-only">{label}</span>
      <div aria-hidden="true">{children}</div>
    </div>
  );
}

/** The backups list: when | file name | size | migration | restore, `count` rows. */
export function BackupRowsSkeleton({ label, count = 5 }: { label: string; count?: number }) {
  return (
    <Shell label={label}>
      <Rows label={label}>
        {Array.from({ length: count }, (_, i) => (
          <Row key={i} dense className="sy-backup-row">
            <span className="sy-backup-when">{bar("3.25rem")}</span>
            <span className="sy-backup-name">{bar("70%")}</span>
            <span className="sy-backup-meta" style={{ justifySelf: "end" }}>
              {bar("3rem")}
            </span>
            <span className="sy-backup-meta">{bar("5.5rem")}</span>
            <div className="sy-backup-action">{bar("9.5rem", { height: BUTTON_HEIGHT, borderRadius: "var(--radius-md)" })}</div>
          </Row>
        ))}
      </Rows>
    </Shell>
  );
}

/** The API tokens and paired devices lists: name | level | when | revoke, `count` rows. */
export function TokenRowsSkeleton({ label, count = 3 }: { label: string; count?: number }) {
  return (
    <Shell label={label}>
      <div className="sy-list-scroll">
        <Rows label={label}>
          {Array.from({ length: count }, (_, i) => (
            <Row key={i} dense className="sy-token-row">
              <span className="sy-token-name">{bar("40%")}</span>
              <span>{bar("4rem")}</span>
              <span className="sy-token-meta">{bar("6rem")}</span>
              <div className="sy-token-action">
                {bar("6.5rem", { height: BUTTON_HEIGHT, borderRadius: "var(--radius-md)" })}
              </div>
            </Row>
          ))}
        </Rows>
      </div>
    </Shell>
  );
}

/** A configuration panel: optional state badges, `facts` key/value lines, optional closing note. */
export function ConfigFactsSkeleton({
  label,
  facts,
  flags = 0,
  note = false,
}: {
  label: string;
  facts: number;
  flags?: number;
  note?: boolean;
}) {
  return (
    <Shell label={label}>
      {flags > 0 && (
        <div className="sy-config-flags">
          {Array.from({ length: flags }, (_, i) => (
            <span key={i} className="ui-skeleton-bar" style={{ width: "4.5rem", height: "1.25rem" }} />
          ))}
        </div>
      )}
      <div className="sy-config-facts">
        {Array.from({ length: facts }, (_, i) => (
          <div key={i} className="sy-fact">
            <span style={{ ...LINE, flex: "none" }}>{bar("6rem")}</span>
            <span style={{ ...LINE, flex: 1, minWidth: 0, justifyContent: "flex-end" }}>{bar("45%")}</span>
          </div>
        ))}
      </div>
      {note && <p className="sy-note" style={LINE}>{bar("60%")}</p>}
    </Shell>
  );
}

/** The PII tally: its real column heads over `count` rows of three bars. */
export function PiiTableSkeleton({ label, count = 5 }: { label: string; count?: number }) {
  return (
    <Shell label={label}>
      <Table label={label}>
        <thead>
          <tr>
            <Th>Column</Th>
            <Th>Class</Th>
            <Th num>Count</Th>
          </tr>
        </thead>
        <tbody>
          {Array.from({ length: count }, (_, i) => (
            <tr key={i}>
              <Td>{bar("70%")}</Td>
              <Td>{bar("50%")}</Td>
              <Td num>{bar("2rem", { marginLeft: "auto" })}</Td>
            </tr>
          ))}
        </tbody>
      </Table>
    </Shell>
  );
}
