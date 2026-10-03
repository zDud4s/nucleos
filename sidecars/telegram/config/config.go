package config

import (
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

const defaultPollInterval = 5 * time.Second

type Config struct {
	DaemonURL      string
	DaemonToken    string
	BotToken       string
	AllowedChatID  int64
	AllowedUserIDs []int64
	PollInterval   time.Duration
	TranscribeCmd  string
	// ProjectTopics maps a project id to the forum thread its feed lines go to. Empty means every
	// line goes to the configured chat.
	ProjectTopics map[string]int64
}

type fileSettings struct {
	AllowedChatID int64 `json:"allowed_chat_id"`
	// AllowedUserIDs is what makes a group chat safe to configure: without it the room's id is the
	// only credential, and a room's membership is not a credential.
	AllowedUserIDs      []int64 `json:"allowed_user_ids"`
	PollIntervalSeconds int     `json:"poll_interval_seconds"`
	TranscribeCmd       string  `json:"transcribe_cmd"`
	// ProjectTopics: project id -> forum thread id. Optional.
	ProjectTopics map[string]int64 `json:"project_topics"`
}

func Load() (Config, error) {
	daemonURL := os.Getenv("NUCLEOS_DAEMON_URL")
	if daemonURL == "" {
		daemonURL = "http://127.0.0.1:8791"
	}
	if err := validateDaemonURL(daemonURL); err != nil {
		return Config{}, err
	}

	daemonToken := os.Getenv("NUCLEOS_DAEMON_TOKEN")
	if daemonToken == "" {
		return Config{}, errors.New("NUCLEOS_DAEMON_TOKEN is required")
	}

	botToken := os.Getenv("TELEGRAM_BOT_TOKEN")
	if botToken == "" {
		return Config{}, errors.New("TELEGRAM_BOT_TOKEN is required")
	}

	configPath := os.Getenv("NUCLEOS_TELEGRAM_CONFIG")
	if configPath == "" {
		configPath = filepath.Join(nucleosDir(), "telegram-config.json")
	}

	settings, err := LoadFromFile(configPath)
	if err != nil {
		return Config{}, err
	}

	pollInterval := time.Duration(settings.PollIntervalSeconds) * time.Second
	if settings.PollIntervalSeconds == 0 {
		pollInterval = defaultPollInterval
	}

	return Config{
		DaemonURL:      daemonURL,
		DaemonToken:    daemonToken,
		BotToken:       botToken,
		AllowedChatID:  settings.AllowedChatID,
		AllowedUserIDs: settings.AllowedUserIDs,
		PollInterval:   pollInterval,
		TranscribeCmd:  settings.TranscribeCmd,
		ProjectTopics:  settings.ProjectTopics,
	}, nil
}

// validateDaemonURL refuses a URL that would carry the daemon token off the machine in cleartext.
// That token authorises approving an agent's actions and flipping the kill switch; the daemon is a
// local process, so plain HTTP is only ever right for loopback.
func validateDaemonURL(raw string) error {
	parsed, err := url.Parse(raw)
	if err != nil {
		return fmt.Errorf("NUCLEOS_DAEMON_URL %q is not a URL: %w", raw, err)
	}
	if parsed.Hostname() == "" {
		return fmt.Errorf("NUCLEOS_DAEMON_URL %q has no host", raw)
	}

	switch parsed.Scheme {
	case "https":
		return nil
	case "http":
		if isLoopback(parsed.Hostname()) {
			return nil
		}
		return fmt.Errorf("NUCLEOS_DAEMON_URL %q would send the daemon token in cleartext to a "+
			"host that is not loopback; use https", raw)
	default:
		return fmt.Errorf("NUCLEOS_DAEMON_URL %q must be http or https", raw)
	}
}

func isLoopback(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func LoadFromFile(path string) (fileSettings, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return fileSettings{}, nil
		}
		return fileSettings{}, fmt.Errorf("read Telegram config %q: %w", path, err)
	}

	var settings fileSettings
	if err := json.Unmarshal(data, &settings); err != nil {
		return fileSettings{}, fmt.Errorf("parse Telegram config %q: %w", path, err)
	}

	for id, thread := range settings.ProjectTopics {
		if thread <= 0 {
			return fileSettings{}, fmt.Errorf("Telegram config %q: project_topics[%q] = %d; a forum thread id must be positive", path, id, thread)
		}
	}

	return settings, nil
}

func (c Config) IsAllowed(chatID int64) bool {
	return c.AllowedChatID != 0 && chatID == c.AllowedChatID
}

// IsAllowedSender answers the question the chat id cannot: who pressed it. In a private chat the
// chat id IS the user's id, so a configured chat already pins one person and no allowlist is
// needed. A group id pins a room instead — every member, current and future, could otherwise
// approve proposals and flip the kill switch, and an inline button is checked against the chat it
// lives in, not the finger that pressed it. Groups therefore require an explicit allowlist.
func (c Config) IsAllowedSender(chatID, userID int64) bool {
	if len(c.AllowedUserIDs) > 0 {
		for _, allowed := range c.AllowedUserIDs {
			if allowed == userID {
				return true
			}
		}
		return false
	}
	return userID != 0 && chatID == userID
}

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
