package main

import (
	"log"
	"time"

	"nucleostelegram/config"
	"nucleostelegram/daemon"
	"nucleostelegram/pipe"
	"nucleostelegram/telegram"
)

func main() {
	cfg, err := config.Load()
	if err != nil {
		log.Fatalf("telegram sidecar config: %v", err)
	}
	dc := daemon.New(cfg.DaemonURL, cfg.DaemonToken)
	bot := telegram.New(cfg.BotToken)
	tr := pipe.NewTracker()

	if cfg.AllowedChatID != 0 {
		go pipe.RunNotifier(bot, dc, cfg.AllowedChatID, cfg.PollInterval)
	} else {
		log.Printf("no allowed_chat_id configured — notifier disabled until one is set")
	}

	log.Printf("telegram sidecar started; long-polling updates")
	var offset int64
	for {
		updates, err := bot.GetUpdates(offset, 50)
		if err != nil {
			log.Printf("getUpdates error: %v", err)
			time.Sleep(2 * time.Second)
			continue
		}
		for _, u := range updates {
			offset = u.UpdateID + 1
			pipe.HandleUpdate(bot, dc, bot, cfg, tr, u)
		}
	}
}
