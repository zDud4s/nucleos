package config

import (
	"os"
	"path/filepath"
	"reflect"
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
	if !reflect.DeepEqual(settings, fileSettings{}) {
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

// Authorising a chat is not the same as authorising a person. In a private chat the two coincide —
// the chat id IS the user id — which is why no allowlist is needed there. A group id belongs to a
// room: every member, including whoever is added tomorrow, would otherwise be able to approve
// proposals and flip the kill switch, and the inline buttons check the chat the button lives in,
// not the finger that pressed it.
func TestOnlyTheConfiguredSenderCanCommandTheBot(t *testing.T) {
	privateChat := Config{AllowedChatID: 42}
	groupChat := Config{AllowedChatID: -100200300}
	groupWithMembers := Config{AllowedChatID: -100200300, AllowedUserIDs: []int64{7, 9}}

	tests := []struct {
		name   string
		config Config
		chatID int64
		userID int64
		want   bool
	}{
		{name: "private chat owner", config: privateChat, chatID: 42, userID: 42, want: true},
		{name: "private chat, no sender attributed", config: privateChat, chatID: 42, userID: 0, want: false},
		{name: "group member without an allowlist", config: groupChat, chatID: -100200300, userID: 7, want: false},
		{name: "group member on the allowlist", config: groupWithMembers, chatID: -100200300, userID: 9, want: true},
		{name: "group member off the allowlist", config: groupWithMembers, chatID: -100200300, userID: 8, want: false},
		{name: "allowlist overrides the private-chat shortcut", config: Config{AllowedChatID: 42, AllowedUserIDs: []int64{7}}, chatID: 42, userID: 42, want: false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.config.IsAllowedSender(tt.chatID, tt.userID); got != tt.want {
				t.Errorf("IsAllowedSender(%d, %d) = %v, want %v", tt.chatID, tt.userID, got, tt.want)
			}
		})
	}
}

func TestAllowedUserIDsAreReadFromTheConfigFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "telegram-config.json")
	contents := []byte(`{"allowed_chat_id":-100,"allowed_user_ids":[7,9]}`)
	if err := os.WriteFile(path, contents, 0o600); err != nil {
		t.Fatalf("write config: %v", err)
	}

	settings, err := LoadFromFile(path)
	if err != nil {
		t.Fatalf("LoadFromFile() error = %v", err)
	}
	if len(settings.AllowedUserIDs) != 2 || settings.AllowedUserIDs[0] != 7 || settings.AllowedUserIDs[1] != 9 {
		t.Errorf("AllowedUserIDs = %v, want [7 9]", settings.AllowedUserIDs)
	}
}

// The daemon token authorises approving an agent's actions. The daemon is a local process and the
// default URL is loopback; a misconfigured host would put that token on the wire in cleartext, so
// anything off-box has to be https.
func TestTheDaemonURLCannotSendTheTokenOffBoxInCleartext(t *testing.T) {
	tests := []struct {
		url     string
		allowed bool
	}{
		{url: "http://127.0.0.1:8791", allowed: true},
		{url: "http://localhost:8791", allowed: true},
		{url: "http://[::1]:8791", allowed: true},
		{url: "https://nucleos.example.com", allowed: true},
		{url: "http://nucleos.example.com", allowed: false},
		{url: "http://192.168.1.10:8791", allowed: false},
		{url: "ftp://127.0.0.1", allowed: false},
		{url: "127.0.0.1:8791", allowed: false},
		{url: "http://", allowed: false},
	}

	for _, tt := range tests {
		t.Run(tt.url, func(t *testing.T) {
			err := validateDaemonURL(tt.url)
			if tt.allowed && err != nil {
				t.Errorf("validateDaemonURL(%q) = %v, want it accepted", tt.url, err)
			}
			if !tt.allowed && err == nil {
				t.Errorf("validateDaemonURL(%q) = nil, want it rejected", tt.url)
			}
		})
	}
}

func TestLoadRejectsACleartextRemoteDaemonURL(t *testing.T) {
	t.Setenv("NUCLEOS_DAEMON_URL", "http://nucleos.example.com")
	t.Setenv("NUCLEOS_DAEMON_TOKEN", "daemon-token")
	t.Setenv("TELEGRAM_BOT_TOKEN", "bot-token")
	t.Setenv("NUCLEOS_TELEGRAM_CONFIG", filepath.Join(t.TempDir(), "missing.json"))

	if _, err := Load(); err == nil {
		t.Fatal("Load() error = nil, want the cleartext remote daemon URL rejected")
	}
}

// The update offset lives on disk so a crash cannot redeliver a batch that was already executed:
// `/kill off` running a second time re-arms an autonomous agent nobody asked to re-arm.
func TestTheUpdateOffsetSurvivesARestart(t *testing.T) {
	path := filepath.Join(t.TempDir(), "telegram-offset")

	if got := LoadOffset(path); got != 0 {
		t.Errorf("LoadOffset(missing) = %d, want 0", got)
	}
	if err := SaveOffset(path, 4242); err != nil {
		t.Fatalf("SaveOffset() error = %v", err)
	}
	if got := LoadOffset(path); got != 4242 {
		t.Errorf("LoadOffset() = %d, want 4242", got)
	}

	if err := os.WriteFile(path, []byte("not a number"), 0o600); err != nil {
		t.Fatalf("write offset: %v", err)
	}
	if got := LoadOffset(path); got != 0 {
		t.Errorf("LoadOffset(corrupt) = %d, want 0 so the bot still starts", got)
	}
}
