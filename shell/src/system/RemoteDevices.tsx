import { useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  useCreatePairingCode,
  useRemoteDevices,
  useRevokeDevice,
  type PairingCode,
  type RemoteDevice,
} from "../data/remote";
import type { ApiTokenLevel } from "../data/system";
import { DAEMON_NAME } from "../copy";
import {
  Badge,
  Button,
  ConfirmButton,
  CopyOnce,
  ErrorNote,
  Field,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  Select,
  Skeleton,
} from "../ui";

const LEVELS: ApiTokenLevel[] = ["admin", "run-creating", "read-only"];

/** What each level lets the paired device do, said beside the choice. */
const LEVEL_HINT: Record<ApiTokenLevel, string> = {
  admin:
    "works like you on this machine, except shutdown, secrets, backups, tokens and remote devices.",
  "run-creating": "reads everything and starts runs.",
  "read-only": "reads only.",
};

/**
 * Pair and revoke remote devices.
 *
 * The pairing code lives in this panel's own state, never in the query cache:
 * the daemon shows it once. `CopyOnce` renders it.
 */
export function RemoteDevices() {
  const devices = useRemoteDevices();
  const createCode = useCreatePairingCode();
  const [name, setName] = useState("");
  const [level, setLevel] = useState<ApiTokenLevel>("admin");
  const [created, setCreated] = useState<PairingCode | null>(null);

  function handleCreate() {
    const trimmed = name.trim();
    if (trimmed === "") return;
    createCode.mutate(
      { device_name: trimmed, access_level: level },
      {
        onSuccess: (code) => {
          setCreated(code);
          setName("");
        },
      },
    );
  }

  return (
    <Panel title="Remote devices">
      <p className="sy-note">
        Pair a browser on another device. Create a code here, then enter it on that device.
      </p>

      {created !== null && (
        <>
          <CopyOnce
            value={created.code}
            label={`the pairing code for ${created.device_name}`}
            onDismiss={() => setCreated(null)}
          />
          <p className="sy-note">
            Valid 5 minutes, single use. On the device, open https://&lt;this machine&gt;.ts.net in
            the browser and enter this code.
          </p>
        </>
      )}

      <form
        className="sy-mint-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!createCode.isPending) handleCreate();
        }}
      >
        <div className="sy-mint-name">
          <Field label="Device name">
            <input
              type="text"
              placeholder="e.g. Phone"
              maxLength={64}
              value={name}
              onChange={(event) => setName(event.target.value)}
            />
          </Field>
        </div>
        <Field label="Access level">
          <Select value={level} onChange={(event) => setLevel(event.target.value as ApiTokenLevel)}>
            {LEVELS.map((candidate) => (
              <option key={candidate} value={candidate}>
                {candidate}
              </option>
            ))}
          </Select>
        </Field>
        <Button
          type="submit"
          variant="ghost"
          disabled={createCode.isPending || name.trim() === ""}
        >
          Create code
        </Button>
      </form>
      <p className="sy-note">
        The {level} level {LEVEL_HINT[level]}
      </p>
      {createCode.isError && <CreateError error={createCode.error} />}

      {devices.isError && devices.data === undefined && (
        <ErrorNote>the paired devices could not be read: the {DAEMON_NAME} is not responding</ErrorNote>
      )}
      {!devices.isError && devices.data === undefined && <Skeleton label="Reading the devices" />}
      {devices.data !== undefined && devices.data.length === 0 && (
        <Quiet says="no device has been paired." />
      )}
      {devices.data !== undefined && devices.data.length > 0 && (
        <Rows label="Remote devices">
          {devices.data.map((device) => (
            <DeviceRow key={device.id} device={device} />
          ))}
        </Rows>
      )}
    </Panel>
  );
}

function CreateError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          remote_disabled: "Remote access is switched off in ~/.nucleos/remote.yaml",
          invalid_device_name: "a device name is 1–64 printable characters",
        }}
      />
    );
  }
  return <ErrorNote>the code was not created: the {DAEMON_NAME} is not responding</ErrorNote>;
}

function DeviceRow({ device }: { device: RemoteDevice }) {
  const revoke = useRevokeDevice();
  const revoked = device.revoked_at !== null;

  return (
    <Row dense className="sy-token-row">
      <span className="sy-token-name" title={device.name}>
        {device.name}
      </span>
      <span>
        <Badge tone="info">{device.access_level}</Badge>
      </span>
      <span className="sy-token-meta">
        {device.revoked_at !== null ? (
          <>
            revoked <RelativeTime at={device.revoked_at} />
          </>
        ) : device.last_seen_at !== null ? (
          <>
            seen <RelativeTime at={device.last_seen_at} />
          </>
        ) : (
          <>
            paired <RelativeTime at={device.created_at} />, never seen
          </>
        )}
      </span>
      <div className="sy-token-action">
        {!revoked && (
          <ConfirmButton
            label="Revoke"
            confirmLabel="Really revoke"
            sayAs={`Revoke ${device.name}`}
            variant="danger"
            disabled={revoke.isPending}
            onConfirm={() => revoke.mutate(device.id)}
          />
        )}
      </div>
      {revoke.isError && (
        <div className="sy-token-error">
          {isApiRefusal(revoke.error) ? (
            <RefusalNote
              refusal={revoke.error}
              sentences={{ not_found: "that device is already gone" }}
            />
          ) : (
            <ErrorNote>that device was not revoked: the {DAEMON_NAME} is not responding</ErrorNote>
          )}
        </div>
      )}
    </Row>
  );
}
