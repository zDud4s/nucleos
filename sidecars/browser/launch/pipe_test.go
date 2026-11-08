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
	"testing"
	"time"
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
//	silent: reads everything and never answers
func fakeBrowser(mode string) {
	if mode == "exit" {
		return
	}
	in, out := fakePipeEnds()
	if in == nil || out == nil {
		os.Exit(2)
	}
	reader := bufio.NewReader(in)
	for {
		raw, err := reader.ReadBytes(0)
		if err != nil {
			return
		}
		if mode != "reply" {
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
