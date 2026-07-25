package transcribe

import (
	"errors"
	"os/exec"
	"strings"
)

// ErrNoTranscriber is returned when no transcribe command is configured.
var ErrNoTranscriber = errors.New("no transcribe command configured")

// Transcribe runs the configured command with the audio file path appended as the final argument
// and returns the trimmed stdout. The command string is whitespace-split into program + args
// (simple splitting is sufficient for a local, user-supplied command). Returns ErrNoTranscriber
// when cmd is empty, or a wrapped error when the command fails.
func Transcribe(cmd, audioPath string) (string, error) {
	fields := strings.Fields(cmd)
	if len(fields) == 0 {
		return "", ErrNoTranscriber
	}
	args := append(fields[1:], audioPath)
	out, err := exec.Command(fields[0], args...).Output()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}
