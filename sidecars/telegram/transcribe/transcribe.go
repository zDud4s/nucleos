package transcribe

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os/exec"
	"strings"
	"time"
)

// ErrNoTranscriber is returned when no transcribe command is configured.
var ErrNoTranscriber = errors.New("no transcribe command configured")

const (
	// A voice note is seconds of audio; a transcriber still running after a minute is stuck, and
	// this is the process that also carries `/kill`. The per-chat queue only serialises work, so
	// this deadline is what actually bounds how long a wedged transcriber can sit on a chat.
	defaultTimeout = time.Minute
	// Enough for any real transcript, small enough that a program printing forever cannot grow
	// this process without bound.
	maxOutputBytes = 1 << 20
	// The child gets a moment to die after its context is cancelled; without it, Wait can block
	// forever on a grandchild still holding the output pipe.
	waitDelay = 5 * time.Second
)

// errOutputCapped stops the copy from the child's stdout once the cap is reached. It never reaches
// a caller: hitting the cap yields a clipped transcript, not a failure.
var errOutputCapped = errors.New("transcriber output cap reached")

// Transcribe runs the configured command with the audio file path appended as the final argument
// and returns the trimmed stdout. The command string is whitespace-split into program + args
// (simple splitting is sufficient for a local, user-supplied command). Returns ErrNoTranscriber
// when cmd is empty, or a wrapped error when the command fails, times out, or cannot be started.
func Transcribe(cmd, audioPath string) (string, error) {
	return transcribeWithin(cmd, audioPath, defaultTimeout, maxOutputBytes)
}

func transcribeWithin(cmd, audioPath string, timeout time.Duration, limit int) (string, error) {
	fields := strings.Fields(cmd)
	if len(fields) == 0 {
		return "", ErrNoTranscriber
	}

	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	args := append(fields[1:], audioPath)
	command := exec.CommandContext(ctx, fields[0], args...)
	out := &cappedBuffer{limit: limit}
	command.Stdout = out
	command.WaitDelay = waitDelay

	err := command.Run()
	switch {
	case out.capped:
		return strings.TrimSpace(out.String()), nil
	case errors.Is(ctx.Err(), context.DeadlineExceeded):
		return "", fmt.Errorf("transcriber gave up after %s", timeout)
	case err != nil:
		return "", err
	}
	return strings.TrimSpace(out.String()), nil
}

type cappedBuffer struct {
	limit  int
	buf    bytes.Buffer
	capped bool
}

func (b *cappedBuffer) Write(p []byte) (int, error) {
	room := b.limit - b.buf.Len()
	if room > 0 {
		if len(p) > room {
			p = p[:room]
		}
		b.buf.Write(p)
		room -= len(p)
	}
	if room <= 0 {
		// Refusing the write closes the pipe, which is what makes the child stop printing instead
		// of being read forever into a buffer that is already full.
		b.capped = true
		return 0, errOutputCapped
	}
	return len(p), nil
}

func (b *cappedBuffer) String() string { return b.buf.String() }
