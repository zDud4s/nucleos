package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// OffsetPath is where the confirmed update offset is kept, next to the config it belongs with.
func OffsetPath() string {
	if configured := os.Getenv("NUCLEOS_TELEGRAM_OFFSET"); configured != "" {
		return configured
	}
	return filepath.Join(nucleosDir(), "telegram-offset")
}

// nucleosDir is where the Telegram config and its update offset live. LOCALAPPDATA comes first and
// unchanged, so Windows resolves exactly where it always has. Elsewhere it is the user config
// directory (XDG_CONFIG_HOME or ~/.config on Linux, ~/Library/Application Support on macOS): joined
// with an unset LOCALAPPDATA the old path was relative to wherever the sidecar was started. The last
// resort is that same relative "nucleos", reached only when the OS names no config directory at all.
func nucleosDir() string {
	if local := os.Getenv("LOCALAPPDATA"); local != "" {
		return filepath.Join(local, "nucleos")
	}
	if dir, err := os.UserConfigDir(); err == nil {
		return filepath.Join(dir, "nucleos")
	}
	return "nucleos"
}

// LoadOffset reads the last confirmed update offset. Anything unreadable reads as 0: starting from
// Telegram's own backlog is recoverable, refusing to start is not.
func LoadOffset(path string) int64 {
	data, err := os.ReadFile(path)
	if err != nil {
		return 0
	}
	offset, err := strconv.ParseInt(strings.TrimSpace(string(data)), 10, 64)
	if err != nil || offset < 0 {
		return 0
	}
	return offset
}

// SaveOffset records the offset before the batch it covers is handled. That ordering is deliberate:
// a crash mid-handling must not redeliver the batch, because re-executing `/kill off` re-arms an
// autonomous agent nobody asked to re-arm.
func SaveOffset(path string, offset int64) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return fmt.Errorf("create offset directory: %w", err)
	}
	if err := os.WriteFile(path, []byte(strconv.FormatInt(offset, 10)), 0o600); err != nil {
		return fmt.Errorf("write update offset: %w", err)
	}
	return nil
}
