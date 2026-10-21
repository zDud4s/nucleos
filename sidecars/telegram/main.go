// Command telegram is the Telegram sidecar on the channel kit (built as telegram-sidecar).
package main

import (
	"context"
	"log"
	"os"
	"os/signal"
	"syscall"

	"nucleoschannelkit/clock"
	"nucleoschannelkit/kit"

	"nucleostelegram/adapter"
	"nucleostelegram/telegram"
)

// The Bot API client must serve every role the adapter plays.
var (
	_ adapter.SendBot = (*telegram.Client)(nil)
	_ adapter.PollBot = (*telegram.Client)(nil)
	_ adapter.FileBot = (*telegram.Client)(nil)
)

func main() {
	cfg, err := loadConfig(os.Getenv)
	if err != nil {
		log.Fatalf("telegram sidecar config: %v", err)
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	bot := telegram.New(cfg.BotToken)
	clk := clock.Real()

	factory := func(c *kit.Client, sink kit.Sink) (kit.Adapter, kit.Ingest, error) {
		attacher := adapter.MediaAttacher{Bot: bot, Up: mediaUploader{c: c}}
		ingest := func(ctx context.Context) error {
			return adapter.Poll(ctx, bot, attacher, &sink, cfg.OffsetPath, clk)
		}
		return adapter.New(bot, c), ingest, nil
	}

	if err := kit.Run(ctx, cfg.Kit, factory); err != nil {
		log.Fatalf("telegram sidecar: %v", err)
	}
}
