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

/**
 * What the owner picks. "owner" is not an API level: it pairs an `admin` device with the owner flag
 * (`owner: true` in the request), which also reaches what only this machine reached.
 */
type Choice = ApiTokenLevel | "owner";

const LEVELS: Choice[] = ["owner", "admin", "run-creating", "read-only"];

/** What each level lets the paired device do, said beside the choice. */
const LEVEL_HINT: Record<Choice, string> = {
  owner:
    "works exactly like you on this machine: also shutdown and restart, secrets, backups, tokens and remote devices.",
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
  const [level, setLevel] = useState<Choice>("admin");
  const [created, setCreated] = useState<PairingCode | null>(null);
  const setup = useRemoteSetup();

  function handleCreate() {
    const trimmed = name.trim();
    if (trimmed === "") return;
    createCode.mutate(
      level === "owner"
        ? { device_name: trimmed, access_level: "admin", owner: true }
        : { device_name: trimmed, access_level: level },
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
          <Select value={level} onChange={(event) => setLevel(event.target.value as Choice)}>
            {LEVELS.map((candidate) => (
              <option key={candidate} value={candidate}>
                {candidate === "owner" ? "Owner" : candidate}
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
        The {level === "owner" ? "Owner" : level} level {LEVEL_HINT[level]}
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
        <div className="sy-list-scroll" tabIndex={0}>
          <Rows label="Remote devices">
            {devices.data.map((device) => (
              <DeviceRow key={device.id} device={device} />
            ))}
          </Rows>
        </div>
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

  // Funnel is the public internet: said whether or not remote access is on, and never offered.
  const exposed = setup.funnel && (
    <ErrorNote>
      Tailscale Funnel has this machine&apos;s HTTPS port open to the public internet. NucleOS does not
      turn Funnel on and will not turn remote access on while it is. Turn Funnel off in Tailscale.
    </ErrorNote>
  );

  if (setup.enabled) {
    return (
      <div className="sy-note">
        <p>
          Remote access is on at <strong>{setup.public_origin}</strong>.
          {setup.serving ? " Tailscale is publishing it." : ""}
        </p>
        {exposed}
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
      {exposed}
      {setup.port_taken && !setup.funnel && (
        <p>
          Tailscale is already serving something else on this machine&apos;s HTTPS port. NucleOS
          leaves it alone, so it cannot publish the page there. Free that port, or use an address you
          already have.
        </p>
      )}
      {ready && (
        <>
          <p>Ready: {ts.dns_name}. Remote access is published on your tailnet only, never the public internet.</p>
          <Button
            variant="ghost"
            disabled={enable.isPending || setup.funnel || setup.port_taken}
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
    // The tailnet never enabled Serve: Tailscale printed a page to approve it on. The link comes
    // from the CLI's own output, and only an https one is ever opened.
    if (error.code === "serve_needs_approval" && error.detail.startsWith("https://")) {
      const approval = error.detail;
      return (
        <>
          <ErrorNote>
            Tailscale needs you to approve Serve for your tailnet first. Open the page, approve it,
            then press Turn on remote access again.
          </ErrorNote>
          <Button variant="ghost" onClick={() => void openUrl(approval)}>
            Open the Tailscale approval page
          </Button>
        </>
      );
    }
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          invalid_origin: "That address cannot be used: it must be https://host with no path.",
          tailscale_not_ready: "Tailscale is not ready: sign in and enable HTTPS certificates.",
          remote_page_missing: "This installation has no remote page to serve.",
          serve_failed: "Tailscale refused to publish the page.",
          serve_needs_approval: "Tailscale needs you to approve Serve for your tailnet first.",
          serve_port_taken:
            "Tailscale is already serving something else on this machine's HTTPS port. NucleOS leaves it alone: free that port, or use an address you already have.",
          funnel_enabled:
            "Tailscale Funnel has this machine open to the public internet. NucleOS will not turn remote access on while it is: turn Funnel off in Tailscale, then try again.",
          not_primary:
            "Another NucleOS on this machine runs the remote gateway. Turn remote access on there.",
          config_unreadable:
            "The remote access settings file cannot be read, so NucleOS did not touch it. Fix or remove remote.yaml in the settings folder, then try again.",
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
        <Badge tone="info">{device.owner ? "Owner" : device.access_level}</Badge>
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
