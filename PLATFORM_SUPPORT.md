# Platform support

What NucleOS does on each operating system, and how much of it has been tried on a real machine.
"Tested" means someone ran it on that system; "not yet measured" means the code is written but
nobody has watched it work there.

| Function | Windows | macOS | Linux |
| --- | --- | --- | --- |
| Start when you log in | Tested on Windows only | Written, not tested on a Mac | Written, not tested on Linux |
| Keep running without a login (Settings, System, Tokens) | Not yet measured; see below | Not available, see below | Not yet measured; see below |
| Do not sleep while work runs (`keep_awake_while_working`, on by default) | Implemented (`SetThreadExecutionState`); not yet watched on a real machine | Written (`caffeinate -i`), not tested on a Mac | Written (`systemd-inhibit --what=idle`), not tested on Linux |

## Keep running without a login

Off by default. When it is on, the background service starts with the machine instead of waiting for
you to log in, so work carries on while nobody is sitting at it.

- **Windows.** The scheduled task starts at boot and runs as your account. Windows needs your
  account password once to allow that; NucleOS passes it to the system tool and does not store or log
  it. An account that signs in only with a PIN or Windows Hello may have no usable password, in which
  case Windows refuses and the page shows Windows' own message. Still to prove on a real machine:
  after a restart with nobody logged in, the service answers, can read its token from the Credential
  Manager, completes a real CLI turn (authentication) and starts the sidecars. The password-less
  `S4U` logon type is not used because it cannot reach the Credential Manager.
- **macOS.** Not available. A LaunchAgent only runs after a login, and a LaunchDaemon cannot reach
  the Keychain where NucleOS keeps its credentials. If you need the machine usable unattended, turn
  on automatic login in the system settings; the page says so. Still to prove: that the agent starts
  and keeps its Keychain access after an automatic login.
- **Linux.** Uses `loginctl enable-linger` on the systemd user unit, so no password is needed. It
  needs a systemd user manager; without one the option is shown as unavailable with the reason. Still
  to prove: on a real distribution, that the unit starts at boot with no session, and that the CLI's
  authentication and the sidecars work without a login.
