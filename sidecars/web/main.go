// Command web-sidecar reads the internet on the núcleo's behalf.
//
// It is the only process in NucleOS that fetches a URL somebody else chose, and it is deliberately
// the dumbest one: it holds no state, opens no database, caches nothing, and makes no decision
// about whether a page may be trusted. Trust is decided in Rust (`core/src/trust.rs`) over the
// FINAL url this process reports, and the cache lives in SQLite, which this program has never been
// able to open. So it can be restarted, killed or replaced mid-request without anything needing to
// be reconciled.
package main

import (
	"log"
	"os"

	"nucleosweb/config"
	"nucleosweb/fetch"
	"nucleosweb/search"
	"nucleosweb/serve"
)

func main() {
	log.SetPrefix("web-sidecar: ")
	// Stateless (see the package comment), so the orderly shutdown when the daemon is gone is to exit.
	watchLifeline(os.Getenv, os.Stdin, func() { os.Exit(0) })
	cfg, err := config.Load()
	if err != nil {
		// The daemon only starts this process once the pillar is enabled, so a missing variable is a
		// wiring bug worth failing loudly on rather than answering every request with an error.
		log.Fatalf("configuration: %v", err)
	}

	provider, err := search.Select(
		cfg.Provider,
		cfg.BraveKey,
		cfg.SearxngURL,
		search.NewClient(cfg.FetchTimeout),
	)
	if err != nil {
		// Not fatal. Reading a page and searching for one are two separate capabilities, and the
		// first has always worked without the second: an installation with no API key can still be
		// handed a URL. `/search` answers 503 and `/fetch` carries on.
		log.Printf("search unavailable: %v — /fetch still works", err)
		provider = search.Unavailable{Reason: err}
	}

	log.Fatal(serve.Serve(cfg, provider, fetch.New(cfg.FetchTimeout, cfg.MaxPageBytes)))
}
