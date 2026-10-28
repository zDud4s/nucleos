// Command remote-sidecar is the browser-facing gateway for remote access to the núcleo.
//
// It holds no state and opens no database. It authenticates nothing itself: it carries a browser's
// session cookie to the daemon, which decides, and it holds only the daemon's `remote` service key,
// never the control token. Everything a browser sends is checked here first (origin, CSRF, fetch
// metadata) and everything it receives carries the security headers of the contract.
package main

import (
	"log"
	"os"

	"nucleosremote/config"
	"nucleosremote/serve"
)

func main() {
	log.SetPrefix("remote-sidecar: ")
	// Stateless, so the orderly shutdown when the daemon is gone is to exit.
	watchLifeline(os.Getenv, os.Stdin, func() { os.Exit(0) })
	cfg, err := config.Load()
	if err != nil {
		// The daemon only starts this process once remote.yaml is valid and enabled, so a bad
		// variable is a wiring bug worth failing loudly on.
		log.Fatalf("configuration: %v", err)
	}
	// Empty handlers mean the real auth, api proxy and static ones (see serve.Serve).
	log.Fatal(serve.Serve(cfg, serve.Handlers{}))
}
