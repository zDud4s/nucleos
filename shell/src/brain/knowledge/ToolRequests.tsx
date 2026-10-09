import { useState } from "react";
import { useTeams } from "../../data/teams";
import {
  useApproveLoadoutTool,
  useLoadoutTools,
  useRejectLoadoutTool,
  type LoadoutTool,
} from "../../data/loadout-tools";
import { Button, Count, Panel, RelativeTime, Row, Rows } from "../../ui";
import { ToolRefusal } from "./ScopedTools";
import "./knowledge.css";

/**
 * The "Tools to approve" queue: what runs asked to be equipped with. The
 * approval goes to the asking owner by default, or to a team — which moves the
 * grant to every agent of that team.
 *
 * `reason` is text a run wrote. It is shown as text and nothing reads it as an
 * instruction.
 */
export function ToolRequests() {
  const requests = useLoadoutTools("proposed");
  const approve = useApproveLoadoutTool();
  const reject = useRejectLoadoutTool();
  const teams = useTeams();

  const rows = Array.isArray(requests.data) ? requests.data : [];
  if (rows.length === 0) return null;

  const deciding = approve.isPending || reject.isPending;
  const refusal = approve.error ?? reject.error;
  const teamIds = (Array.isArray(teams.data) ? teams.data : []).map((team) => team.id);

  return (
    <>
      {refusal !== null && <ToolRefusal error={refusal} />}
      <Panel title="Tools to approve" aside={<Count n={rows.length} />}>
        <p className="learned-lede">
          None is callable until you approve it; a refusal stays on the record.
        </p>
        <Rows label="Tool requests">
          {rows.map((row) => (
            <RequestRow
              key={row.id}
              row={row}
              teamIds={teamIds}
              deciding={deciding}
              onApprove={(team) => approve.mutate({ id: row.id, target: team })}
              onReject={() => reject.mutate(row.id)}
            />
          ))}
        </Rows>
      </Panel>
    </>
  );
}

function RequestRow({
  row,
  teamIds,
  deciding,
  onApprove,
  onReject,
}: {
  row: LoadoutTool;
  teamIds: string[];
  deciding: boolean;
  onApprove: (team: { team: string } | null) => void;
  onReject: () => void;
}) {
  // "" is the asking owner itself; anything else is a team id.
  const [team, setTeam] = useState("");

  return (
    <Row className="learned-row">
      <div className="learned-head">
        <span className="learned-scope">{row.tool}</span>
        <span>
          {row.owner_kind} {row.owner_id}
        </span>
        {row.run_id !== null && <span>run {row.run_id}</span>}
        <span className="learned-when">
          <RelativeTime at={row.created_at} />
        </span>
      </div>
      {row.reason !== null && row.reason !== "" && <p className="learned-title">{row.reason}</p>}
      <div className="learned-group-head">
        <span className="learned-scope-chooser">
          <select
            aria-label={`Approve ${row.tool} for`}
            value={team}
            onChange={(event) => setTeam(event.target.value)}
            disabled={deciding}
          >
            <option value="">
              {row.owner_kind} {row.owner_id}
            </option>
            {teamIds.map((id) => (
              <option key={id} value={id}>
                team {id}
              </option>
            ))}
          </select>
        </span>
        <Button
          variant="approve"
          aria-label={`Approve ${row.tool}`}
          onClick={() => onApprove(team === "" ? null : { team })}
          disabled={deciding}
        >
          Approve
        </Button>
        <Button aria-label={`Refuse ${row.tool}`} onClick={onReject} disabled={deciding}>
          Refuse
        </Button>
      </div>
    </Row>
  );
}
