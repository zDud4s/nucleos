// §spec pilar-de-browser

package launch

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
)

// Process is a running Chromium and the debugging port it reported.
type Process struct {
	cmd  *exec.Cmd
	Port int
	// ProfileDir is kept so Stop knows which tree it is reaping.
	ProfileDir string
}

// portFile is where Chromium writes the port it actually chose.
const portFile = "DevToolsActivePort"

// ErrInheritedInstance means the launch did not start a browser of its own.
//
// SPIKE FINDING, and it cost a whole phase of the spike before anyone noticed. Starting Chromium
// over a profile that another instance still holds does NOT fail: the new process hands off to the
// survivor and exits, so the caller sees a successful spawn and then drives somebody else's
// browser — one that may have no fence attached. Detecting it is why DevToolsActivePort is deleted
// before the launch and required to reappear after: only a genuinely new instance writes it.
var ErrInheritedInstance = errors.New("launch: chromium did not start a new instance; the profile is held by a surviving process")

// Start launches Chromium and waits for it to report its debugging port.
func Start(ctx context.Context, opts Options, wait time.Duration) (*Process, error) {
	args, err := Args(opts)
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(opts.ProfileDir, 0o755); err != nil {
		return nil, fmt.Errorf("creating profile dir: %w", err)
	}
	// Before the browser starts, because a preference read at startup is only read at startup. This
	// is the only thing that closes spec §6.2b — no command-line flag does — so a launch that could
	// not write it is a launch with a hole in the fence, and refusing is the same answer ErrNoProxy
	// gives for the same reason.
	if err := applyWebRTCPolicy(opts.ProfileDir, opts.Mode); err != nil {
		return nil, err
	}
	marker := filepath.Join(opts.ProfileDir, portFile)
	// Delete first. See ErrInheritedInstance: a stale file would make an inherited instance look
	// like a fresh one, which is the failure this whole dance exists to catch.
	if err := os.Remove(marker); err != nil && !os.IsNotExist(err) {
		return nil, fmt.Errorf("clearing %s: %w", portFile, err)
	}

	cmd := exec.CommandContext(ctx, opts.ExecutablePath, args...)
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("starting chromium: %w", err)
	}

	process := &Process{cmd: cmd, ProfileDir: opts.ProfileDir}
	port, err := waitForPort(marker, wait)
	if err != nil {
		// Reap whatever did start, so a failed launch does not leave a browser behind holding the
		// profile — which would make the NEXT launch inherit it.
		process.Stop()
		return nil, err
	}
	process.Port = port
	return process, nil
}

func waitForPort(marker string, wait time.Duration) (int, error) {
	deadline := time.Now().Add(wait)
	for time.Now().Before(deadline) {
		raw, err := os.ReadFile(marker)
		if err == nil {
			lines := strings.Split(strings.TrimSpace(string(raw)), "\n")
			if port, err := strconv.Atoi(strings.TrimSpace(lines[0])); err == nil && port > 0 {
				return port, nil
			}
		}
		time.Sleep(50 * time.Millisecond)
	}
	return 0, ErrInheritedInstance
}

// DebuggerURL asks the browser where its WebSocket endpoint is.
//
// Deliberately with Proxy: nil. This is our own control channel to a port that asks for no token at
// all, and it is the one thing on loopback the fence refuses to let a page reach (spec §6.2). Sending
// it through the fence's own proxy would either fail or, worse, make the proxy the thing that decides
// whether the driver can talk to its browser.
func DebuggerURL(port int, timeout time.Duration) (string, error) {
	client := &http.Client{Timeout: timeout, Transport: &http.Transport{Proxy: nil}}
	response, err := client.Get(fmt.Sprintf("http://127.0.0.1:%d/json/version", port))
	if err != nil {
		return "", fmt.Errorf("asking the browser for its debugger url: %w", err)
	}
	defer response.Body.Close()
	var payload struct {
		WebSocketDebuggerURL string `json:"webSocketDebuggerUrl"`
	}
	if err := json.NewDecoder(response.Body).Decode(&payload); err != nil {
		return "", fmt.Errorf("reading the browser's version payload: %w", err)
	}
	if payload.WebSocketDebuggerURL == "" {
		return "", errors.New("launch: the browser reported no debugger url")
	}
	return payload.WebSocketDebuggerURL, nil
}

// Stop reaps the whole process tree.
//
// SPIKE FINDING: killing the pid we launched is not enough. Chromium hands the session to processes
// the launcher does not own, so the parent dies, the browser keeps running with our argv, the
// profile stays locked, and the next launch silently inherits it (see ErrInheritedInstance). Spec
// §9.3 says "mata-se a árvore" and this is the line that has to mean it.
func (p *Process) Stop() {
	if p == nil || p.cmd == nil || p.cmd.Process == nil {
		return
	}
	pid := p.cmd.Process.Pid
	switch runtime.GOOS {
	case "windows":
		// /T is the whole point: it takes the descendants too.
		kill := exec.Command("taskkill", "/T", "/F", "/PID", strconv.Itoa(pid))
		_ = kill.Run()
	default:
		// Negative pid signals the process group, which is the same idea on POSIX.
		_ = exec.Command("kill", "-9", "--", "-"+strconv.Itoa(pid)).Run()
	}
	_ = p.cmd.Process.Kill()
	_, _ = p.cmd.Process.Wait()
}
