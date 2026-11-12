package launch

import (
	"os/exec"
	"strconv"
)

// ownProcessGroup does nothing on Windows: taskkill /T finds the descendants by walking the tree.
func ownProcessGroup(cmd *exec.Cmd) {}

// killGroup ends the whole tree. /T is the whole point: it takes the descendants too. This is the
// command Stop ran on Windows before process groups were used on Unix, unchanged.
func killGroup(pid int) {
	kill := exec.Command("taskkill", "/T", "/F", "/PID", strconv.Itoa(pid))
	_ = kill.Run()
}
