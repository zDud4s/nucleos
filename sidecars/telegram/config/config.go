package config

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

const defaultPollInterval = 5 * time.Second

type Config struct {
	DaemonURL     string
	DaemonToken   string
	BotToken      string
	AllowedChatID int64
	PollInterval  time.Duration
	TranscribeCmd string
}

type fileSettings struct {
	AllowedChatID       int64  `json:"allowed_chat_id"`
	PollIntervalSeconds int    `json:"poll_interval_seconds"`
	TranscribeCmd       string `json:"transcribe_cmd"`
}

func Load() (Config, error) {
	daemonURL := os.Getenv("NUCLEOS_DAEMON_URL")
	if daemonURL == "" {
		daemonURL = "http://127.0.0.1:8791"
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
		configPath = filepath.Join(os.Getenv("LOCALAPPDATA"), "nucleos", "telegram-config.json")
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
		DaemonURL:     daemonURL,
		DaemonToken:   daemonToken,
		BotToken:      botToken,
		AllowedChatID: settings.AllowedChatID,
		PollInterval:  pollInterval,
		TranscribeCmd: settings.TranscribeCmd,
	}, nil
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

	return settings, nil
}

func (c Config) IsAllowed(chatID int64) bool {
	return c.AllowedChatID != 0 && chatID == c.AllowedChatID
}
