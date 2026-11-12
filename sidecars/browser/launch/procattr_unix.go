//go:build unix

package launch

import (
	"os/exec"
	"syscall"
)

// ownProcessGroup makes the command the leader of a new process group. Every helper Chromium
// spawns inherits that group, so Stop can end all of them with one signal. Without it the browser
// sat in this sidecar's own group, and the group Stop named did not exist.
func ownProcessGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
}

// killGroup sends SIGKILL to the process group pid leads, through the syscall rather than the
// kill(1) binary: how that binary parses a negative pid varies, and procps-ng 4.0.4 (Ubuntu 24.04)
// was measured on 2026-09-12 reading `kill -KILL -<pgid>` as every process of the user. A pid of 0
// or 1 would name this sidecar's own group or every process, so neither is ever signalled.
func killGroup(pid int) {
	if pid <= 1 {
		return
	}
	_ = syscall.Kill(-pid, syscall.SIGKILL)
}
