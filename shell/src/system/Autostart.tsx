import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useAutostart, useSetAutostart, type AutostartMode } from "../data/system";
import { DAEMON_NAME } from "../copy";
import { Button, ErrorNote, Field, RefusalNote, Segmented } from "../ui";
import { PrefRow } from "../pages/PrefRow";

/**
 * When the daemon starts: at login (the default) or at boot, without anyone logged in.
 *
 * A machine-settings row; it sits inside a `Rows`. What the choice can do depends on the platform and
 * the daemon says so: on Windows "At boot" needs the account's password once, which lives in this
 * row's state until the call settles and is never cached, stored or shown again.
 */
export function Autostart() {
  const autostart = useAutostart();
  const change = useSetAutostart();
  const [password, setPassword] = useState("");
  const [refused, setRefused] = useState<unknown>(null);
  const [askPassword, setAskPassword] = useState(false);
  const view = autostart.data;

  function apply(mode: AutostartMode) {
    const body = mode === "at-boot" && password !== "" ? { mode, password } : { mode };
    setRefused(null);
    change.mutate(body, {
      onError: (error) => setRefused(error),
      // Whatever the outcome, neither the field nor the mutation's stored variables keep the
      // password; the refusal, which has none in it, is kept in this panel's own state.
      onSuccess: () => setAskPassword(false),
      onSettled: () => {
        setPassword("");
        change.reset();
      },
    });
  }

  function choose(next: AutostartMode) {
    if (view === undefined) return;
    if (next === view.mode) {
      setAskPassword(false);
      return;
    }
    // Where Windows needs the account's password, the choice waits for it in the form below.
    if (next === "at-boot" && view.needs_password) {
      setAskPassword(true);
      return;
    }
    apply(next);
  }

  const note =
    autostart.isError && view === undefined ? (
      <ErrorNote>the start-up setting could not be read: the {DAEMON_NAME} is not responding</ErrorNote>
    ) : view !== undefined ? (
      <>
        {!view.at_boot && view.why_not !== null && <p className="sy-note">{view.why_not}</p>}
        {askPassword && view.at_boot && view.mode === "at-login" && view.needs_password && (
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
        {refused !== null && <ChangeError error={refused} />}
      </>
    ) : null;

  return (
    <PrefRow
      label="Start with Windows"
      tipName="About starting with Windows"
      tip={`At boot starts the ${DAEMON_NAME} when the PC powers on, before anyone signs in. At login starts it when you sign in.`}
      note={note}
    >
      {view === undefined ? (
        autostart.isError ? null : (
          <div role="status" aria-busy="true">
            <span className="sr-only">Reading the start-up setting</span>
            <span className="ui-skeleton-bar sy-pref-skeleton" aria-hidden="true" />
          </div>
        )
      ) : view.at_boot ? (
        <Segmented
          label="Start with Windows"
          value={view.mode}
          options={[
            { value: "at-login", label: "At login", disabled: change.isPending },
            { value: "at-boot", label: "At boot", disabled: change.isPending },
          ]}
          onChange={choose}
        />
      ) : (
        <span className="sy-prefrow-value">{view.mode === "at-boot" ? "At boot" : "At login"}</span>
      )}
    </PrefRow>
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
