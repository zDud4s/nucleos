// Command browser-sidecar drives a real browser on the núcleo's behalf.
//
// It is the only process in NucleOS that renders somebody else's HTML and runs their JavaScript. It
// holds no state, opens no database, and decides no trust: which profile a session runs in, and
// whether a host may be visited at all, are decided in Rust (`core/src/browser_policy.rs`) before
// this process is called. So it can be restarted, killed or replaced mid-session without anything
// needing to be reconciled — the cost is the session, never the identity.
//
// # Born disabled
//
// The pillar ships with `enabled: false` and stays there until the fence (spec §14.2 step E) and
// the tools (step I) are green together. Until then the daemon does not start this process at all.
package main

import (
	"log"

	"nucleosbrowser/browser"
	"nucleosbrowser/config"
	"nucleosbrowser/serve"
)

func main() {
	log.SetPrefix("browser-sidecar: ")
	cfg, err := config.Load()
	if err != nil {
		// The daemon only starts this process once the pillar is enabled, so a missing variable is
		// a wiring bug worth failing loudly on rather than answering every request with an error.
		log.Fatalf("configuration: %v", err)
	}

	driver, err := selectDriver(cfg)
	if err != nil {
		// Fatal, unlike the web sidecar's search provider. There, a missing API key still left
		// /fetch working: two capabilities, one of them unaffected. Here there is one capability,
		// and a browser sidecar with no browser has nothing left to answer — so it says so once at
		// startup instead of turning every request into a 503 nobody reads.
		log.Fatalf("driver %q: %v", cfg.Driver, err)
	}

	log.Fatal(serve.Serve(cfg, driver))
}

// selectDriver builds the configured implementation.
//
// Only "fake" exists today, and that is not a placeholder: spec §14.2 puts the contract (step A)
// before the implementation (step D) precisely so this switch can be written before anyone knows
// what goes in it. The second gate of §14.1a decides whether the real entry is PinchTab, go-rod or
// raw CDP; whichever it is, it lands here and nothing above this line changes.
func selectDriver(cfg config.Config) (browser.Driver, error) {
	switch cfg.Driver {
	case "fake":
		// Fenced, because a fake that refused everything would make the sidecar untestable
		// end-to-end. The real drivers attach a real fence and must fail closed without one.
		return &browser.Fake{FenceAttached: true}, nil
	default:
		return nil, browser.ErrUnsupported
	}
}
