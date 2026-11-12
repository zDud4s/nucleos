// §spec pilar-de-browser

package launch

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"nucleosbrowser/cdp"
)

// TestMain turns this test binary into a fake browser when GO_FAKE_CHROME is set. It runs before any
// flag parsing, because the launcher hands the fake Chromium-style flags the testing package rejects.
func TestMain(m *testing.M) {
	if mode := os.Getenv("GO_FAKE_CHROME"); mode != "" {
		fakeBrowser(mode)
		os.Exit(0)
	}
	os.Exit(m.Run())
}

// fakeBrowser speaks the DevTools pipe protocol: NUL-terminated JSON in both directions.
//
//	reply:  answers every message with a result carrying the same id, until the pipe closes
//	exit:   exits at once without answering (Chrome handing off to a surviving instance)
//	hold:   answers like reply, but stays up with its end of the pipe open after the pipe closes
//	fail:   exits with status 3 without answering (a browser that crashed or refused to start)
//	silent: reads everything and never answers
func fakeBrowser(mode string) {
	switch mode {
	case "exit":
		return
	case "fail":
		os.Exit(3)
	}
	in, out := fakePipeEnds()
	if in == nil || out == nil {
		os.Exit(2)
	}
	reader := bufio.NewReader(in)
	for {
		raw, err := reader.ReadBytes(0)
		if err != nil {
			if mode == "hold" {
				time.Sleep(time.Minute)
			}
			return
		}
		if mode != "reply" && mode != "hold" {
			continue
		}
		var msg struct {
			ID int64 `json:"id"`
		}
		if err := json.Unmarshal(raw[:len(raw)-1], &msg); err != nil {
			continue
		}
		reply := fmt.Sprintf(`{"id":%d,"result":{"product":"fake"}}`, msg.ID)
		if _, err := out.Write(append([]byte(reply), 0)); err != nil {
			return
		}
	}
}

func startFake(t *testing.T, mode string, wait time.Duration) (*Process, string, error) {
	t.Helper()
	t.Setenv("GO_FAKE_CHROME", mode)
	profile := t.TempDir()
	process, err := StartArgs(context.Background(), os.Args[0], nil, profile, wait)
	if process != nil {
		t.Cleanup(process.Stop)
	}
	return process, profile, err
}

func TestStartHandsBackWorkingPipeEnds(t *testing.T) {
	process, _, err := startFake(t, "reply", 10*time.Second)
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	r, w := process.Pipe()
	if _, err := w.Write(append([]byte(`{"id":7,"method":"Browser.getVersion"}`), 0)); err != nil {
		t.Fatalf("write: %v", err)
	}
	type result struct {
		raw []byte
		err error
	}
	done := make(chan result, 1)
	go func() {
		raw, err := bufio.NewReader(r).ReadBytes(0)
		done <- result{raw, err}
	}()
	select {
	case got := <-done:
		if got.err != nil {
			t.Fatalf("read: %v", got.err)
		}
		var msg struct {
			ID int64 `json:"id"`
		}
		if err := json.Unmarshal(got.raw[:len(got.raw)-1], &msg); err != nil {
			t.Fatalf("reply is not JSON: %v (%q)", err, got.raw)
		}
		if msg.ID != 7 {
			t.Fatalf("reply id = %d, want 7", msg.ID)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("no reply over the pipe")
	}
}

func TestStartReturnsErrInheritedInstanceWhenTheLaunchedProcessExits(t *testing.T) {
	began := time.Now()
	_, _, err := startFake(t, "exit", 10*time.Second)
	if !errors.Is(err, ErrInheritedInstance) {
		t.Fatalf("got %v, want ErrInheritedInstance", err)
	}
	if elapsed := time.Since(began); elapsed > 8*time.Second {
		t.Fatalf("took %v; an exited process must be noticed well before the wait", elapsed)
	}
}

func TestStartTimesOutWhenTheBrowserNeverAnswers(t *testing.T) {
	began := time.Now()
	_, _, err := startFake(t, "silent", time.Second)
	if err == nil {
		t.Fatal("start succeeded against a browser that never answers")
	}
	if elapsed := time.Since(began); elapsed > 5*time.Second {
		t.Fatalf("took %v, want about the 1s wait", elapsed)
	}
}

func TestStartOpensNoDevToolsActivePortFile(t *testing.T) {
	_, profile, err := startFake(t, "reply", 10*time.Second)
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	if _, err := os.Stat(filepath.Join(profile, "DevToolsActivePort")); !os.IsNotExist(err) {
		t.Fatalf("DevToolsActivePort exists or stat failed unexpectedly: %v", err)
	}
}

func TestStartReportsTheExitStatusWhenTheLaunchedProcessFails(t *testing.T) {
	_, _, err := startFake(t, "fail", 10*time.Second)
	if err == nil {
		t.Fatal("start succeeded against a browser that exited with an error")
	}
	if errors.Is(err, ErrInheritedInstance) {
		t.Fatalf("a failed exit was reported as an inherited instance: %v", err)
	}
	var exited *ExitedError
	if !errors.As(err, &exited) {
		t.Fatalf("got %T %v, want *ExitedError", err, err)
	}
	if exited.Code != 3 {
		t.Fatalf("exit code = %d, want 3", exited.Code)
	}
	if !strings.Contains(err.Error(), "3") {
		t.Fatalf("the message must carry the status: %q", err)
	}
}

func TestStartKeepsTheTimeoutErrorForASilentLiveBrowser(t *testing.T) {
	_, _, err := startFake(t, "silent", 500*time.Millisecond)
	if !errors.Is(err, errNoPipeReply) {
		t.Fatalf("got %v, want errNoPipeReply", err)
	}
	if errors.Is(err, ErrInheritedInstance) {
		t.Fatalf("a live browser must not read as an inherited instance: %v", err)
	}
}

func TestProbeIdCannotCollideWithConnIds(t *testing.T) {
	// cdp.Conn numbers its commands 1, 2, 3...; the probe runs before a Conn exists but its reply id
	// must stay out of that range so a late duplicate could never be matched to a real call.
	if probeID < 1<<32 {
		t.Fatalf("probeID = %d, too close to the Conn's own counter", probeID)
	}
	if !strings.Contains(probeRequest, fmt.Sprintf(`"id":%d,`, probeID)) {
		t.Fatalf("probeRequest %q does not use probeID %d", probeRequest, probeID)
	}
}

func readLoops() int {
	buf := make([]byte, 1<<20)
	n := runtime.Stack(buf, true)
	return strings.Count(string(buf[:n]), "cdp.(*Conn).readLoop")
}

func TestClosingTheConnReleasesTheReaderWhilePeerIsOpen(t *testing.T) {
	before := readLoops()
	process, _, err := startFake(t, "hold", 10*time.Second)
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	r, w := process.Pipe()
	conn := cdp.NewPipe(r, w)
	// The browser is alive and holds its end open: only Close can end the reader.
	started := time.Now().Add(2 * time.Second)
	for readLoops() != before+1 {
		if time.Now().After(started) {
			t.Fatalf("readLoop goroutines = %d, want %d while the conn is open", readLoops(), before+1)
		}
		time.Sleep(10 * time.Millisecond)
	}
	_ = conn.Close()
	deadline := time.Now().Add(3 * time.Second)
	for readLoops() > before {
		if time.Now().After(deadline) {
			t.Fatalf("the reader goroutine is still blocked 3s after Close (%d, baseline %d)", readLoops(), before)
		}
		time.Sleep(20 * time.Millisecond)
	}
}
