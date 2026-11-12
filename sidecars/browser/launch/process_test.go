// §spec pilar-de-browser

package launch

import (
	"os"
	"os/exec"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// TestHelperProcess is the child the Stop tests start: it only sleeps until it is killed.
func TestHelperProcess(t *testing.T) {
	if os.Getenv("GO_WANT_HELPER_PROCESS") != "1" {
		return
	}
	time.Sleep(time.Minute)
}

// countKillGroup swaps killGroupFn for a counter that does not signal anything, so a test can never
// reap a pid that is not its own.
func countKillGroup(t *testing.T) *int32 {
	t.Helper()
	var calls int32
	previous := killGroupFn
	killGroupFn = func(int) { atomic.AddInt32(&calls, 1) }
	t.Cleanup(func() { killGroupFn = previous })
	return &calls
}

func startHelper(t *testing.T) *exec.Cmd {
	t.Helper()
	cmd := exec.Command(os.Args[0], "-test.run=TestHelperProcess")
	cmd.Env = append(os.Environ(), "GO_WANT_HELPER_PROCESS=1")
	if err := cmd.Start(); err != nil {
		t.Fatalf("starting the helper process: %v", err)
	}
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		_, _ = cmd.Process.Wait()
	})
	return cmd
}

func TestStopTwiceSignalsTheGroupOnce(t *testing.T) {
	calls := countKillGroup(t)
	process := &Process{cmd: startHelper(t)}

	process.Stop()
	process.Stop()

	if got := atomic.LoadInt32(calls); got != 1 {
		t.Fatalf("killGroup ran %d times across two Stop calls, want 1: a second call would signal a pid the OS may have reused", got)
	}
}

func TestConcurrentStopsSignalOnce(t *testing.T) {
	calls := countKillGroup(t)
	process := &Process{cmd: startHelper(t)}

	var group sync.WaitGroup
	for i := 0; i < 8; i++ {
		group.Add(1)
		go func() {
			defer group.Done()
			process.Stop()
		}()
	}
	group.Wait()

	if got := atomic.LoadInt32(calls); got != 1 {
		t.Fatalf("killGroup ran %d times across 8 concurrent Stop calls, want 1", got)
	}
}

func TestStopOfAnAlreadyWaitedProcessSendsNothing(t *testing.T) {
	calls := countKillGroup(t)
	cmd := exec.Command(os.Args[0], "-test.run=TestHelperProcessExits")
	cmd.Env = append(os.Environ(), "GO_WANT_HELPER_PROCESS=1")
	if err := cmd.Start(); err != nil {
		t.Fatalf("starting the helper process: %v", err)
	}
	_ = cmd.Wait()
	process := &Process{cmd: cmd}

	process.Stop()

	if got := atomic.LoadInt32(calls); got != 0 {
		t.Fatalf("killGroup ran %d times for a process that was already waited, want 0", got)
	}
}

// TestHelperProcessExits is a child that returns at once, so Wait has something to reap.
func TestHelperProcessExits(t *testing.T) {}
