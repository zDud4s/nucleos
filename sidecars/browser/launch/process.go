// §spec pilar-de-browser

package launch

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"slices"
	"sync"
	"time"
)

// Process is a running Chromium and the DevTools pipe it is driven over.
//
// There is no debugging port: the browser listens on nothing, so only the process holding the pipe
// ends can drive it. The consequence is by design — an external DevTools attach (chrome://inspect,
// remote debugging by a person) is not possible on the sidecar's browser.
type Process struct {
	cmd *exec.Cmd
	// ProfileDir is kept so Stop knows which tree it is reaping.
	ProfileDir string

	// pipes holds the parent's ends (nil in tests that build a bare Process). reader wraps the read
	// end and outlives the probe, so no byte the browser sent after its first reply is lost.
	pipes  *devtoolsPipes
	reader *bufio.Reader

	// stopOnce makes Stop idempotent: the first call waits the child, which frees its pid, so any later
	// call would signal a pid the OS may already have handed to an unrelated program.
	stopOnce sync.Once
}

// killGroupFn is killGroup behind a variable so tests can count signals without sending any.
var killGroupFn = killGroup

// ErrInheritedInstance means the launch did not start a browser of its own.
//
// SPIKE FINDING, and it cost a whole phase of the spike before anyone noticed. Starting Chromium
// over a profile that another instance still holds does NOT fail: the new process hands off to the
// survivor and exits, so the caller sees a successful spawn and then drives somebody else's
// browser — one that may have no fence attached. With the DevTools pipe the signal is that the
// launched process exits, and the pipe reads EOF, before the first successful reply: only a genuinely
// new instance answers on the pipe it was given.
var ErrInheritedInstance = errors.New("launch: chromium did not start a new instance; the profile is held by a surviving process")

// errNoPipeReply means the browser stayed up but never answered on the pipe within the wait.
var errNoPipeReply = errors.New("launch: the browser never answered on the DevTools pipe")

// probeRequest is the first message sent: it proves the pipe is live and the browser is ours.
const probeRequest = `{"id":1,"method":"Browser.getVersion"}`

// Start launches Chromium and waits for it to answer on the DevTools pipe.
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
	return StartArgs(ctx, opts.ExecutablePath, args, opts.ProfileDir, wait)
}

// StartArgs launches exe with args, wires the DevTools pipe to it and probes it with a first command.
// Start is this plus argv and profile setup. --remote-debugging-pipe is appended here when args does
// not already carry it (Args puts it there itself), so the browser always has a DevTools transport;
// the caller's slice is never mutated.
func StartArgs(ctx context.Context, exe string, args []string, profileDir string, wait time.Duration) (*Process, error) {
	pipes, err := newDevtoolsPipes()
	if err != nil {
		return nil, fmt.Errorf("creating the devtools pipes: %w", err)
	}
	full := slices.Clone(args)
	if !slices.Contains(full, "--remote-debugging-pipe") {
		full = append(full, "--remote-debugging-pipe")
	}
	cmd := exec.CommandContext(ctx, exe, full...)
	// Its own process group on Unix, so Stop can take every helper it spawns at once.
	ownProcessGroup(cmd)
	// Suspended on Windows, so joinJob can put it in this sidecar's kill-on-close job before it has
	// spawned a single helper: see job_windows.go. A no-op elsewhere.
	startSuspended(cmd)
	// After both of the above: on Unix ownProcessGroup replaces SysProcAttr, on Windows startSuspended
	// sets the CreationFlags that wire must preserve.
	cmd.Args = append(cmd.Args, pipes.wire(cmd)...)

	err = cmd.Start()
	// The browser has its own copies now (or never will). Closing ours is what makes its exit
	// observable as EOF on the read end.
	_ = pipes.browserRead.Close()
	_ = pipes.browserWrite.Close()
	if err != nil {
		_ = pipes.parentWrite.Close()
		_ = pipes.parentRead.Close()
		return nil, fmt.Errorf("starting chromium: %w", err)
	}
	if err := joinJob(cmd); err != nil {
		// Never resumed, so it has spawned nothing: the pid is the whole tree.
		_ = cmd.Process.Kill()
		_, _ = cmd.Process.Wait()
		_ = pipes.parentWrite.Close()
		_ = pipes.parentRead.Close()
		return nil, err
	}

	process := &Process{cmd: cmd, ProfileDir: profileDir, pipes: pipes, reader: bufio.NewReader(pipes.parentRead)}
	if err := process.probe(ctx, wait); err != nil {
		// Reap whatever did start, so a failed launch does not leave a browser behind holding the
		// profile — which would make the NEXT launch inherit it.
		process.Stop()
		return nil, err
	}
	return process, nil
}

// probe sends one command and waits for the first reply.
func (p *Process) probe(ctx context.Context, wait time.Duration) error {
	result := make(chan error, 1)
	go func() {
		if _, err := p.pipes.parentWrite.Write(append([]byte(probeRequest), 0)); err != nil {
			result <- err
			return
		}
		_, err := p.reader.ReadBytes(0)
		result <- err
	}()
	timer := time.NewTimer(wait)
	defer timer.Stop()
	select {
	case err := <-result:
		if err != nil {
			// The pipe broke before any reply: the process we started is gone.
			return ErrInheritedInstance
		}
		return nil
	case <-timer.C:
		return errNoPipeReply
	case <-ctx.Done():
		return ctx.Err()
	}
}

// Pipe returns the parent's ends of the DevTools pipe: the browser's output (buffered, already past the
// probe reply) and the writer for commands. Hand them to cdp.NewPipe.
func (p *Process) Pipe() (io.Reader, io.WriteCloser) {
	return p.reader, p.pipes.parentWrite
}

// Stop reaps the whole process tree.
//
// SPIKE FINDING: killing the pid we launched is not enough. Chromium hands the session to processes
// the launcher does not own, so the parent dies, the browser keeps running with our argv, the
// profile stays locked, and the next launch silently inherits it (see ErrInheritedInstance). Spec
// §9.3 says "mata-se a árvore" and this is the line that has to mean it.
func (p *Process) Stop() {
	if p == nil {
		return
	}
	p.stopOnce.Do(p.stop)
}

func (p *Process) stop() {
	// After the tree is dead, so a reader blocked on the pipe is released by EOF as well as by Close.
	defer p.closePipes()
	if p.cmd == nil || p.cmd.Process == nil {
		return
	}
	// Already waited by another path: the pid is free and may belong to someone else now.
	if p.cmd.ProcessState != nil {
		return
	}
	pid := p.cmd.Process.Pid
	// The group Start made Chromium lead on Unix, or the whole tree on Windows: see killGroup in
	// procattr_unix.go and procattr_windows.go.
	killGroupFn(pid)
	_ = p.cmd.Process.Kill()
	_, _ = p.cmd.Process.Wait()
}

// closePipes closes the parent's ends; nil-safe for a Process built without pipes.
func (p *Process) closePipes() {
	if p.pipes == nil {
		return
	}
	_ = p.pipes.parentWrite.Close()
	_ = p.pipes.parentRead.Close()
}
