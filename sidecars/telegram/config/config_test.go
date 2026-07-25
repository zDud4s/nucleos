package config

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestLoadFromFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "telegram-config.json")
	contents := []byte(`{"allowed_chat_id":1234,"poll_interval_seconds":17,"transcribe_cmd":"transcribe --stdin"}`)
	if err := os.WriteFile(path, contents, 0o600); err != nil {
		t.Fatalf("write config: %v", err)
	}

	settings, err := LoadFromFile(path)
	if err != nil {
		t.Fatalf("LoadFromFile() error = %v", err)
	}
	if settings.AllowedChatID != 1234 {
		t.Errorf("AllowedChatID = %d, want 1234", settings.AllowedChatID)
	}
	if settings.PollIntervalSeconds != 17 {
		t.Errorf("PollIntervalSeconds = %d, want 17", settings.PollIntervalSeconds)
	}
	if settings.TranscribeCmd != "transcribe --stdin" {
		t.Errorf("TranscribeCmd = %q, want %q", settings.TranscribeCmd, "transcribe --stdin")
	}
}

func TestLoadMissingFileUsesDefaults(t *testing.T) {
	path := filepath.Join(t.TempDir(), "missing.json")

	settings, err := LoadFromFile(path)
	if err != nil {
		t.Fatalf("LoadFromFile() error = %v", err)
	}
	if settings != (fileSettings{}) {
		t.Errorf("LoadFromFile() = %+v, want zero-value settings", settings)
	}

	t.Setenv("NUCLEOS_DAEMON_URL", "")
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "daemon-token")
	t.Setenv("TELEGRAM_BOT_TOKEN", "bot-token")
	t.Setenv("NUCLEOS_TELEGRAM_CONFIG", path)

	cfg, err := Load()
	if err != nil {
		t.Fatalf("Load() error = %v", err)
	}
	if cfg.DaemonURL != "http://127.0.0.1:8791" {
		t.Errorf("DaemonURL = %q, want default", cfg.DaemonURL)
	}
	if cfg.AllowedChatID != 0 {
		t.Errorf("AllowedChatID = %d, want 0", cfg.AllowedChatID)
	}
	if cfg.PollInterval != 5*time.Second {
		t.Errorf("PollInterval = %s, want 5s", cfg.PollInterval)
	}
	if cfg.TranscribeCmd != "" {
		t.Errorf("TranscribeCmd = %q, want empty", cfg.TranscribeCmd)
	}
}

func TestConfigIsAllowed(t *testing.T) {
	tests := []struct {
		name   string
		config Config
		chatID int64
		want   bool
	}{
		{name: "zero allows nobody", config: Config{}, chatID: 0, want: false},
		{name: "matching id allowed", config: Config{AllowedChatID: 42}, chatID: 42, want: true},
		{name: "other id denied", config: Config{AllowedChatID: 42}, chatID: 7, want: false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.config.IsAllowed(tt.chatID); got != tt.want {
				t.Errorf("IsAllowed(%d) = %v, want %v", tt.chatID, got, tt.want)
			}
		})
	}
}
