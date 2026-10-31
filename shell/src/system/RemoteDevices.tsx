import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  useCreatePairingCode,
  useDisableRemote,
  useEnableRemote,
  useRemoteDevices,
  useRemoteSetup,
  useRevokeDevice,
  type PairingCode,
  type RemoteDevice,
  type RemoteSetup,
} from "../data/remote";
import { QrCode } from "./QrCode";
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
  const setup = useRemoteSetup();

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
      {setup.data !== undefined && <SetupWizard setup={setup.data} />}

      {created !== null && (
        <>
          <CopyOnce
            value={created.code}
            label={`the pairing code for ${created.device_name}`}
            onDismiss={() => setCreated(null)}
          />
          <p className="sy-note">
            Valid 5 minutes, single use. On the device, open{" "}
            {setup.data?.public_origin ? setup.data.public_origin : "this machine's remote address"}{" "}
            in the browser and enter this code.
          </p>
          {setup.data?.enabled === true && setup.data.public_origin !== "" && (
            <QrCode
              value={setup.data.public_origin}
              label={`QR code for ${setup.data.public_origin}`}
            />
          )}
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

const TAILSCALE_DOWNLOAD = "https://tailscale.com/download";
const TAILSCALE_ADMIN_DNS = "https://login.tailscale.com/admin/dns";

/**
 * Turns remote access on without a terminal: Tailscale when it is there and ready, or an HTTPS
 * address the owner already has. It says only what it can see, so it never claims a platform.
 */
function SetupWizard({ setup }: { setup: RemoteSetup }) {
  const enable = useEnableRemote();
  const disable = useDisableRemote();
  const [own, setOwn] = useState("");
  const ts = setup.tailscale;
  const ready = ts.installed && ts.signed_in && ts.https_ok && ts.dns_name !== null;

  if (setup.enabled) {
    return (
      <div className="sy-note">
        <p>
          Remote access is on at <strong>{setup.public_origin}</strong>.
          {setup.serving ? " Tailscale is publishing it." : ""}
        </p>
        <Button
          variant="ghost"
          disabled={disable.isPending}
          onClick={() => disable.mutate()}
        >
          Turn off remote access
        </Button>
        {disable.isError && <ErrorNote>remote access was not turned off</ErrorNote>}
      </div>
    );
  }

  return (
    <div className="sy-note">
      {!ts.installed && (
        <>
          <p>Tailscale is not installed on this machine.</p>
          <Button variant="ghost" onClick={() => void openUrl(TAILSCALE_DOWNLOAD)}>
            Install Tailscale
          </Button>
        </>
      )}
      {ts.installed && !ts.signed_in && (
        <p>Tailscale is installed but not signed in. Open Tailscale, sign in, then reopen this page.</p>
      )}
      {ts.installed && ts.signed_in && !ts.https_ok && (
        <>
          <p>
            Your tailnet has HTTPS certificates switched off. Turn on MagicDNS and HTTPS
            certificates in the Tailscale admin console, then come back.
          </p>
          <Button variant="ghost" onClick={() => void openUrl(TAILSCALE_ADMIN_DNS)}>
            Open the Tailscale admin console
          </Button>
        </>
      )}
      {ready && (
        <>
          <p>Ready: {ts.dns_name}. Remote access is published on your tailnet only, never the public internet.</p>
          <Button
            variant="ghost"
            disabled={enable.isPending}
            onClick={() => enable.mutate(null)}
          >
            Turn on remote access
          </Button>
        </>
      )}
      <form
        className="sy-mint-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!enable.isPending && own.trim() !== "") enable.mutate(own.trim());
        }}
      >
        <div className="sy-mint-name">
          <Field label="Or an HTTPS address you already have">
            <input
              type="text"
              placeholder="https://nucleos.example.com"
              value={own}
              onChange={(event) => setOwn(event.target.value)}
            />
          </Field>
        </div>
        <Button type="submit" variant="ghost" disabled={enable.isPending || own.trim() === ""}>
          Use this address
        </Button>
      </form>
      {enable.isError && <EnableError error={enable.error} />}
    </div>
  );
}

function EnableError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          invalid_origin: "That address cannot be used: it must be https://host with no path.",
          tailscale_not_ready: "Tailscale is not ready: sign in and enable HTTPS certificates.",
          remote_page_missing: "This installation has no remote page to serve.",
          serve_failed: "Tailscale refused to publish the page.",
          no_machine_config: "This daemon has no settings folder to write to.",
        }}
      />
    );
  }
  return <ErrorNote>remote access was not turned on: the {DAEMON_NAME} is not responding</ErrorNote>;
}

function CreateError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          remote_disabled: "Remote access is switched off. Turn it on above, then create the code.",
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
