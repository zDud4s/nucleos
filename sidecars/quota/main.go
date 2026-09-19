// Command quota-sidecar reads how much of each assistant's usage limit the owner has burned.
//
// It exists because the núcleo may not open a connection off this machine (core/AGENTS.md), and one
// of the two providers it reads is a remote endpoint. Like the web sidecar it is deliberately dumb:
// it holds no database, persists nothing, and makes no decision about what a number means. Whether
// a reading is alarming, whether it should warn, and whether it may stop work are all decided in
// Rust (`core/src/quota.rs`) against thresholds the owner sets.
//
// It handles the owner's Claude token for the length of one outbound request and never writes it
// anywhere — see package claude. That is the whole reason the token does not live in NucleOS's own
// credential store: we read another application's credential at the point of use rather than taking
// custody of it (design D2).
package main

import (
	"log"
	"os"

	"nucleosquota/config"
	"nucleosquota/serve"
)

func main() {
	log.SetPrefix("quota-sidecar: ")
	// Stateless, so the orderly shutdown when the daemon is gone is simply to exit.
	watchLifeline(os.Getenv, os.Stdin, func() { os.Exit(0) })

	cfg, err := config.Load()
	if err != nil {
		// The daemon only starts this process once the pillar is enabled, so a missing variable is a
		// wiring bug worth failing loudly on rather than answering every request with an error.
		log.Fatalf("configuration: %v", err)
	}

	home, err := os.UserHomeDir()
	if err != nil {
		// Both providers are read out of the home directory. Without it there is nothing this
		// process could answer, and saying so once beats answering "unmeasured" for ever.
		log.Fatalf("home directory: %v", err)
	}

	log.Fatal(serve.Serve(cfg, serve.Live(cfg, home)))
}
