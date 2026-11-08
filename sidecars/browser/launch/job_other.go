//go:build !windows

package launch

import "os/exec"

// startSuspended does nothing off Windows: there is no job object to assign before the browser
// runs. The process group ownProcessGroup creates is what holds the tree together there, for Stop
// to signal; nothing here reaps that group if this sidecar dies without calling Stop.
func startSuspended(cmd *exec.Cmd) {}

// joinJob does nothing off Windows; see startSuspended and job_windows.go.
func joinJob(cmd *exec.Cmd) error { return nil }
