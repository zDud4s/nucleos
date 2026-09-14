package transcribe

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestHelperProcess(t *testing.T) {
	if os.Getenv("GO_WANT_HELPER_PROCESS") != "1" {
		return
	}

	args := os.Args
	for i, arg := range args {
		if arg == "--" {
			args = args[i+1:]
			break
		}
	}

	switch os.Getenv("HELPER_MODE") {
	case "echo":
		fmt.Println(strings.Join(args, " "))
	case "sleep":
		time.Sleep(30 * time.Second)
	case "print":
		fmt.Println(os.Getenv("HELPER_TEXT"))
	}
	os.Exit(0)
}

func helperCommand(t *testing.T, mode string) string {
	t.Helper()

	dir := t.TempDir()
	name := "helper" + filepath.Ext(os.Args[0])
	helper := filepath.Join(dir, name)
	bytes, err := os.ReadFile(os.Args[0])
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(helper, bytes, 0o755); err != nil {
		t.Fatal(err)
	}
	t.Chdir(dir)
	t.Setenv("GO_WANT_HELPER_PROCESS", "1")
	t.Setenv("HELPER_MODE", mode)
	return "." + string(filepath.Separator) + name + " -test.run=^TestHelperProcess$ --"
}

func TestTranscribeWithoutCommand(t *testing.T) {
	_, err := Transcribe("", "x.ogg")
	if !errors.Is(err, ErrNoTranscriber) {
		t.Fatalf("Transcribe(empty command) error = %v, want ErrNoTranscriber", err)
	}
}

func TestTranscribeRunsConfiguredCommand(t *testing.T) {
	got, err := Transcribe(helperCommand(t, "echo"), "hello.ogg")
	if err != nil {
		t.Fatalf("Transcribe() error = %v", err)
	}
	if got != "hello.ogg" {
		t.Errorf("Transcribe() = %q, want %q", got, "hello.ogg")
	}
}

// The transcriber is an arbitrary user-supplied program running on the path that also carries
// `/kill`. One that hangs used to hang the sidecar with it: no deadline, so the update loop waited
// forever while the notifier kept announcing events — a bot that looks alive with a dead command
// path. The deadline is what bounds that.
func TestATranscriberThatNeverFinishesIsKilled(t *testing.T) {
	start := time.Now()
	_, err := transcribeWithin(helperCommand(t, "sleep"), "127.0.0.1", 150*time.Millisecond, maxOutputBytes)
	if err == nil {
		t.Fatal("transcribeWithin() error = nil, want the deadline reported")
	}
	if elapsed := time.Since(start); elapsed > 10*time.Second {
		t.Errorf("transcribeWithin() returned after %s, want it to give up at the deadline", elapsed)
	}
}

// A transcriber that never stops printing is read straight into this process's memory. The cap is
// what keeps a broken one from taking the sidecar down with it; a clipped transcript is still worth
// relaying, so it is not treated as a failure.
func TestOutputIsCappedRatherThanReadWithoutLimit(t *testing.T) {
	got, err := transcribeWithin(helperCommand(t, "echo"), "0123456789abcdef", time.Minute, 4)
	if err != nil {
		t.Fatalf("transcribeWithin() error = %v, want clipped output to be usable", err)
	}
	if got != "0123" {
		t.Errorf("transcribeWithin() = %q, want the first 4 bytes only", got)
	}
}
