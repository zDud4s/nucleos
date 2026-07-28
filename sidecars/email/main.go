// Command email-sidecar reads one IMAP mailbox and hands what is new to the núcleo.
//
// It is deliberately the dumbest process in the system: it holds no state, writes nothing to the
// mailbox, and makes no decision about what any message means. The cursor lives in the núcleo's
// database and the triage rules live in Rust, so this program can be restarted, killed or replaced
// at any moment without anything needing to be reconciled.
package main

import (
	"log"

	"nucleosemail/config"
	"nucleosemail/daemon"
	"nucleosemail/poll"
)

func main() {
	log.SetPrefix("email-sidecar: ")
	cfg, err := config.Load()
	if err != nil {
		// The daemon only starts this process once the pillar is configured, so a missing variable
		// is a wiring bug worth failing loudly on rather than polling forever without credentials.
		log.Fatalf("configuration: %v", err)
	}

	log.Printf("watching %s on %s as %s every %s",
		cfg.Mailbox, cfg.Addr(), cfg.Username, cfg.PollInterval)
	poll.Run(cfg, daemon.New(cfg.DaemonURL, cfg.DaemonToken))
}
