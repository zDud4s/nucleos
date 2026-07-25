package transcribe

import (
	"errors"
	"testing"
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
