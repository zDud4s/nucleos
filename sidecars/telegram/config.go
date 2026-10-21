package main

import (
	"context"
	"errors"
	"io"
	"strings"

	"nucleoschannelkit/kit"

	"nucleostelegram/config"
)

const (
	envBotToken   = "TELEGRAM_BOT_TOKEN"
	envOffsetPath = "NUCLEOS_TELEGRAM_OFFSET"

	// defaultChannelID is the channel id used when the daemon does not set one.
	defaultChannelID = "telegram"

	// channelKind is the kind announced in the hello.
	channelKind = "telegram"
)

// appConfig is everything the v2 entry point needs. It deliberately does not read telegram.yaml:
// allowlists, project topics and transcription belong to the core in the v2 design.
type appConfig struct {
	Kit        kit.Config
	BotToken   string
	OffsetPath string
}

// loadConfig reads the configuration from the environment. No returned error ever contains the bot
// token.
func loadConfig(getenv func(string) string) (appConfig, error) {
	token := strings.TrimSpace(getenv(envBotToken))
	if token == "" {
		return appConfig{}, errors.New(envBotToken + " is required")
	}
	kc, err := kit.ConfigFromEnv(getenv)
	if err != nil {
		return appConfig{}, err
	}
	if kc.Channel == "" {
		kc.Channel = defaultChannelID
	}
	kc.Kind = channelKind
	offset := getenv(envOffsetPath)
	if offset == "" {
		// Same file as the v1 sidecar, so the switch between them keeps the cursor.
		offset = config.OffsetPath()
	}
	return appConfig{Kit: kc, BotToken: token, OffsetPath: offset}, nil
}

// mediaUploader adapts kit.Client.UploadMedia to the adapter's Uploader interface.
type mediaUploader struct{ c *kit.Client }

func (u mediaUploader) Upload(ctx context.Context, mime string, r io.Reader) (string, error) {
	return u.c.UploadMedia(ctx, mime, r)
}
