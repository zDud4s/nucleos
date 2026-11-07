import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useAutostart, useSetAutostart, type AutostartMode } from "../data/system";
import { DAEMON_NAME } from "../copy";
import { Badge, Button, ErrorNote, Field, Panel, RefusalNote, Skeleton } from "../ui";

/**
 * Keep the daemon running without anyone logged in.
 *
 * Off by default. What the switch can do depends on the platform and the daemon says so: on Windows
 * it needs the account's password once, which lives in this panel's state until the call settles
 * and is never cached, stored or shown again.
 */
export function Autostart() {
  const autostart = useAutostart();
  const change = useSetAutostart();
  const [password, setPassword] = useState("");
  const [refused, setRefused] = useState<unknown>(null);
  const view = autostart.data;

  function apply(mode: AutostartMode) {
    const body = mode === "at-boot" && password !== "" ? { mode, password } : { mode };
    setRefused(null);
    change.mutate(body, {
      onError: (error) => setRefused(error),
      // Whatever the outcome, neither the field nor the mutation's stored variables keep the
      // password; the refusal, which has none in it, is kept in this panel's own state.
      onSettled: () => {
        setPassword("");
        change.reset();
      },
    });
  }

  return (
    <Panel title="Run without a login">
      {autostart.isError && view === undefined && (
        <ErrorNote>the start-up setting could not be read: the {DAEMON_NAME} is not responding</ErrorNote>
      )}
      {!autostart.isError && view === undefined && <Skeleton label="Reading the start-up setting" />}
      {view !== undefined && (
        <>
          <p className="sy-note">
            {view.mode === "at-boot" ? (
              <Badge tone="active">on</Badge>
            ) : (
              <Badge tone="off">off</Badge>
            )}{" "}
            {view.mode === "at-boot"
              ? `The ${DAEMON_NAME} starts with this machine, before anyone logs in.`
              : `The ${DAEMON_NAME} starts when you log in.`}
          </p>

          {!view.at_boot && view.why_not !== null && <p className="sy-note">{view.why_not}</p>}

          {view.at_boot && view.mode === "at-login" && view.needs_password && (
            <form
              className="sy-mint-form"
              onSubmit={(event) => {
                event.preventDefault();
                if (!change.isPending && password !== "") apply("at-boot");
              }}
            >
              <Field
                label="Windows password"
                helper="Windows asks for it once to run the task without a login. It is not saved."
              >
                <input
                  type="password"
                  autoComplete="off"
                  value={password}
                  onChange={(event) => setPassword(event.target.value)}
                />
              </Field>
              <Button type="submit" variant="ghost" disabled={change.isPending || password === ""}>
                Turn on
              </Button>
            </form>
          )}
          {view.at_boot && view.mode === "at-login" && !view.needs_password && (
            <Button variant="ghost" disabled={change.isPending} onClick={() => apply("at-boot")}>
              Turn on
            </Button>
          )}
          {view.at_boot && view.mode === "at-boot" && (
            <Button variant="ghost" disabled={change.isPending} onClick={() => apply("at-login")}>
              Turn off
            </Button>
          )}
        </>
      )}
      {refused !== null && <ChangeError error={refused} />}
    </Panel>
  );
}

function ChangeError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) {
    return (
      <RefusalNote
        refusal={error}
        sentences={{
          password_required: "enter the Windows password for this account",
        }}
      />
    );
  }
  return <ErrorNote>the setting was not changed: the {DAEMON_NAME} is not responding</ErrorNote>;
}
