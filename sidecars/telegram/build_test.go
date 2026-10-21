package main

import (
	"os/exec"
	"path/filepath"
	"testing"
)

// TestMainBuildsAsTheSidecarBinary guards the module root: build-sidecars.sh builds `.` as
// telegram-sidecar, so the root package must stay a buildable main.
func TestMainBuildsAsTheSidecarBinary(t *testing.T) {
	if testing.Short() {
		t.Skip("builds the binary")
	}
	out := filepath.Join(t.TempDir(), "telegram-sidecar")
	cmd := exec.Command("go", "build", "-o", out, ".")
	if b, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("go build: %v\n%s", err, b)
	}
}
