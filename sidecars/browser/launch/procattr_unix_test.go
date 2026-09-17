//go:build unix

package launch

import (
	"bufio"
	"io"
	"os"
	"os/exec"
	"strings"
	"syscall"
	"testing"
	"time"
)

func TestAStartedChromiumLeadsItsOwnProcessGroup(t *testing.T) {
	cmd := exec.Command("sleep", "30")
	ownProcessGroup(cmd)
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	// One pid, never a group: a regressed group kill in a cleanup could take the test runner down.
	defer func() {
		_ = cmd.Process.Kill()
		_, _ = cmd.Process.Wait()
	}()
	pgid, err := syscall.Getpgid(cmd.Process.Pid)
	if err != nil || pgid != cmd.Process.Pid {
		t.Fatalf("pgid = %d (%v), want %d: the browser does not lead a group of its own", pgid, err, cmd.Process.Pid)
	}
}

func TestStoppingTheProcessGroupReapsAGrandchild(t *testing.T) {
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	// The background sleep stands in for a helper Chromium spawned. It inherits the write end, so
	// EOF on r arrives only once every holder, the grandchild included, is gone.
	cmd := exec.Command("sh", "-c", "sleep 30 & echo up; wait")
	cmd.Stdout = w // an *os.File is handed to the child as is, so Wait runs no copy loop on it
	ownProcessGroup(cmd)
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	w.Close()
	lines := bufio.NewReader(r)
	up := make(chan string, 1)
	go func() {
		line, _ := lines.ReadString('\n')
		up <- line
	}()
	select {
	case line := <-up:
		if strings.TrimSpace(line) != "up" {
			t.Fatalf("the stand-in said %q, want %q", line, "up")
		}
	case <-time.After(10 * time.Second):
		_ = cmd.Process.Kill()
		t.Fatal("the stand-in never started its grandchild")
	}

	(&Process{cmd: cmd}).Stop()

	done := make(chan struct{})
	go func() {
		_, _ = io.Copy(io.Discard, lines)
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("a grandchild outlived Stop")
	}
}
