package transcribe

import (
	"errors"
	"runtime"
	"testing"
	"time"
)

func TestTranscribeWithoutCommand(t *testing.T) {
	_, err := Transcribe("", "x.ogg")
	if !errors.Is(err, ErrNoTranscriber) {
		t.Fatalf("Transcribe(empty command) error = %v, want ErrNoTranscriber", err)
	}
}

func TestTranscribeRunsConfiguredCommand(t *testing.T) {
	got, err := Transcribe("cmd /c echo", "hello.ogg")
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
	if runtime.GOOS != "windows" {
		t.Skip("blocks using a Windows shell command")
	}

	// The audio path is simply the last argument, so here it is the ping target: `ping -n 30
	// 127.0.0.1` blocks for about half a minute.
	start := time.Now()
	_, err := transcribeWithin("cmd /c ping -n 30", "127.0.0.1", 150*time.Millisecond, maxOutputBytes)
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
	if runtime.GOOS != "windows" {
		t.Skip("runs a Windows shell command")
	}

	got, err := transcribeWithin("cmd /c echo", "0123456789abcdef", time.Minute, 4)
	if err != nil {
		t.Fatalf("transcribeWithin() error = %v, want clipped output to be usable", err)
	}
	if got != "0123" {
		t.Errorf("transcribeWithin() = %q, want the first 4 bytes only", got)
	}
}
