package main

import (
	"io"
	"log"
	"os"
	"os/signal"
	"syscall"
)

// lifelineVar is set by the daemon on every sidecar it supervises (core/src/sidecar.rs,
// LIFELINE_VAR). Copied, not shared: each sidecar is its own Go module with no common package, so
// the five copies of this file (echo, email, web, telegram, browser) must stay identical.
const lifelineVar = "NUCLEOS_LIFELINE"

// sigpipeCaught is where an armed lifeline sends this process's SIGPIPEs, and nothing reads it. A full
// buffer only drops the notification (os/signal never blocks on it); the runtime has already counted
// the signal as handled. Package-level so that arming twice registers one channel, not two.
var sigpipeCaught = make(chan os.Signal, 1)

// watchLifeline reads stdin to its end when the daemon asked for a lifeline, then calls cut.
//
// The daemon holds the write end of this pipe for as long as it lives. However it dies - orderly,
// SIGKILL, TerminateProcess, a crash - the kernel closes that end and this read returns, which is
// how a sidecar learns it has been orphaned before it can sit on its port forever. A read error is
// a cut line too: nothing on the other end is listening either way.
//
// Without the variable nothing is read and false is returned: a sidecar started by hand, with stdin
// at /dev/null, must not take that EOF as its cue to leave.
//
// SIGPIPE is caught, not ignored, once the lifeline is armed. The daemon held this process's stdout
// and stderr too, so after it dies every log line is a write to a broken pipe, and on Unix the Go
// runtime ends a program that writes to a broken stdout or stderr unless SIGPIPE is ignored or
// delivered to a channel. Without this the first line logged on the way out would kill the shutdown
// it announces. Ignoring it would harm every program this sidecar starts: an ignored signal survives
// exec, and a transcriber printing past its output cap relies on dying of SIGPIPE. A caught one is
// reset to the default in the child. On Windows a broken pipe is only a write error, and the call is
// harmless.
func watchLifeline(getenv func(string) string, stdin io.Reader, cut func()) bool {
	if getenv(lifelineVar) != "1" {
		return false
	}
	signal.Notify(sigpipeCaught, syscall.SIGPIPE)
	go func() {
		if _, err := io.Copy(io.Discard, stdin); err != nil {
			log.Printf("lifeline read failed (%v); treating it as cut", err)
		} else {
			log.Print("lifeline cut: the daemon is gone; shutting down")
		}
		cut()
	}()
	return true
}
