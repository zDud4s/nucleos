// §spec pilar-de-browser

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
	"context"
	"log"
	"os"
	"os/signal"
	"syscall"

	"nucleosbrowser/browser"
	"nucleosbrowser/config"
	"nucleosbrowser/launch"
	"nucleosbrowser/pool"
	"nucleosbrowser/profile"
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

	driver, shutdown, err := selectDriver(cfg)
	if err != nil {
		// Fatal, unlike the web sidecar's search provider. There, a missing API key still left
		// /fetch working: two capabilities, one of them unaffected. Here there is one capability,
		// and a browser sidecar with no browser has nothing left to answer — so it says so once at
		// startup instead of turning every request into a 503 nobody reads.
		log.Fatalf("driver %q: %v", cfg.Driver, err)
	}
	defer shutdown()

	// Spec §9.3: the process this sidecar spawns is not the browser, and a sidecar that exits
	// without reaping leaves a Chrome running with our argv, holding the profile — which the next
	// launch then inherits in silence. A signal handler is what makes the ordinary case (the daemon
	// stopping us) go through the graceful path rather than the inherited one.
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	go func() {
		<-stop
		log.Print("stopping: closing browsers and sweeping ephemeral profiles")
		shutdown()
		os.Exit(0)
	}()
	// The lifeline (spec D3): a daemon that is gone takes the same graceful path a signal takes.
	// Non-blocking because a real signal may already fill the one-slot channel, and then that path
	// is running anyway.
	watchLifeline(os.Getenv, os.Stdin, func() {
		select {
		case stop <- os.Interrupt:
		default:
		}
	})

	log.Fatal(serve.Serve(cfg, driver))
}

// selectDriver builds the configured implementation, and returns how to take it down again.
//
// "fake" is not a placeholder that outstayed its welcome: spec §14.2 put the contract (step A)
// before the implementation (step D) so that this switch could be written before anyone knew what
// went in it, and the answer changed twice during the spike without this file moving.
func selectDriver(cfg config.Config) (browser.Driver, func(), error) {
	switch cfg.Driver {
	case "fake":
		// Fenced, because a fake that refused everything would make the sidecar untestable
		// end-to-end. The real drivers attach a real fence and must fail closed without one.
		return &browser.Fake{FenceAttached: true}, func() {}, nil

	case "chrome":
		install := launch.Install{Root: cfg.Root, Pin: launch.DefaultPin()}
		executable := cfg.ExecutablePath
		if executable == "" {
			executable = install.ExecutablePath()
		}
		build := func() (deferrable, func(), error) { return chromePool(cfg, install, executable) }

		if _, err := os.Stat(executable); err == nil {
			return mustBuild(build)
		}

		// Spec §9.5: the download happens when the pillar is ACTIVATED, which is now — the daemon
		// only starts this process once `.ai/browser.yaml` says so. It runs in the background and
		// this process serves immediately, answering every request with a refusal that names the
		// revision, the path and why the last attempt failed. That is §9.5's "indisponível com a
		// razão", and it is why this is not a fatal error the way an unknown driver is.
		deferred := newDeferredDriver(notInstalled(install, "not downloaded yet"))
		fetching, cancel := context.WithCancel(context.Background())
		go fetchUntilInstalled(fetching, install, deferred, build)
		return deferred, func() {
			cancel()
			deferred.stop()
		}, nil

	default:
		return nil, nil, browser.ErrUnsupported
	}
}

// chromePool builds the pool over an installed Chromium.
func chromePool(cfg config.Config, install launch.Install, executable string) (deferrable, func(), error) {
	store := profile.Store{Root: install.ProfilesDir()}
	// Before anything opens. Every ephemeral profile on disk at this moment is a leftover from a
	// crash, because this process is the only thing that creates them and it holds no state across
	// restarts (spec §9.3).
	if swept, err := store.SweepEphemeral(); err != nil {
		log.Printf("sweeping orphaned profiles: %v", err)
	} else if swept > 0 {
		log.Printf("swept %d ephemeral profile(s) left by an earlier crash", swept)
	}

	browsers := pool.New(
		pool.ChromeLauncher{ExecutablePath: executable, CacheMB: cfg.CacheMB},
		store,
		profile.Limits{
			MaxProjects: cfg.MaxProfiles,
			DiskBudget:  cfg.DiskBudgetMB << 20,
		},
		cfg.MaxSessions,
	)
	return browsers, func() { browsers.Shutdown(context.Background()) }, nil
}

func mustBuild(build func() (deferrable, func(), error)) (browser.Driver, func(), error) {
	driver, stop, err := build()
	if err != nil {
		return nil, nil, err
	}
	return driver, stop, nil
}
