package main

import (
	"context"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	"nucleostelegram/config"
	"nucleostelegram/daemon"
	"nucleostelegram/pipe"
	"nucleostelegram/telegram"
)

// shutdownGrace is how long in-flight updates get to finish once a stop is asked for. An approval
// already on its way to the daemon should land rather than be cut in half.
const shutdownGrace = 15 * time.Second

func main() {
	cfg, err := config.Load()
	if err != nil {
		log.Fatalf("telegram sidecar config: %v", err)
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	dc := daemon.New(cfg.DaemonURL, cfg.DaemonToken)
	bot := telegram.New(cfg.BotToken)
	tr := pipe.NewTracker()
	dispatcher := pipe.NewDispatcher()

	if cfg.AllowedChatID != 0 {
		// Supervised, not bare: a panic in the notifier used to take the process down and the
		// command path with it, silently.
		go pipe.Supervise("notifier", func() {
			// The bare chat and no topic: what the notifier announces — a new proposal, the kill
			// switch, a budget alert — is about the machine and not about any one errand, so it
			// belongs where the whole room sees it rather than buried in whichever topic was open.
			pipe.RunNotifier(ctx, bot, dc, telegram.Destination{ChatID: cfg.AllowedChatID}, cfg.PollInterval)
		})
	} else {
		log.Printf("no allowed_chat_id configured — notifier disabled until one is set")
	}

	offsetPath := config.OffsetPath()
	offset := config.LoadOffset(offsetPath)

	log.Printf("telegram sidecar started; long-polling updates from offset %d", offset)
	for ctx.Err() == nil {
		updates, err := bot.GetUpdatesContext(ctx, offset, 50)
		if err != nil {
			if ctx.Err() != nil {
				break
			}
			log.Printf("getUpdates error: %v", err)
			if !sleepUntil(ctx, 2*time.Second) {
				break
			}
			continue
		}
		if len(updates) == 0 {
			continue
		}

		for _, u := range updates {
			offset = u.UpdateID + 1
		}
		// The batch is confirmed before it is handled, on purpose. Telegram redelivers anything not
		// confirmed, so a crash halfway through would re-execute whatever was in flight — and
		// `/kill off` re-executed re-arms an autonomous agent nobody asked to re-arm. Losing an
		// update to a crash is the safer direction for this particular channel.
		if err := config.SaveOffset(offsetPath, offset); err != nil {
			log.Printf("could not persist update offset: %v", err)
		}

		for _, u := range updates {
			dispatcher.Dispatch(pipe.ChatKey(pipe.DestinationOf(u)), func() {
				pipe.HandleUpdate(bot, dc, bot, cfg, tr, u)
			})
		}
	}

	log.Printf("stopping; finishing updates already in flight")
	dispatcher.Shutdown(shutdownGrace)
}

func sleepUntil(ctx context.Context, d time.Duration) bool {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}
