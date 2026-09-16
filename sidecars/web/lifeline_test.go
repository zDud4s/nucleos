package main

import (
	"errors"
	"io"
	"os/signal"
	"runtime"
	"syscall"
	"testing"
	"time"
)

// askedFor answers getenv the way the environment of a daemon-supervised sidecar does.
func askedFor(key string) string {
	if key == lifelineVar {
		return "1"
	}
	return ""
}

func TestTheLifelineIsIgnoredUnlessTheDaemonAskedForIt(t *testing.T) {
	r, w := io.Pipe()
	defer w.Close()
	cut := func() { t.Error("cut without a lifeline") }
	if watchLifeline(func(string) string { return "" }, r, cut) {
		t.Fatal("watchLifeline started without NUCLEOS_LIFELINE=1")
	}
}

func TestCuttingTheLifelineShutsDown(t *testing.T) {
	r, w := io.Pipe()
	done := make(chan struct{})
	if !watchLifeline(askedFor, r, func() { close(done) }) {
		t.Fatal("watchLifeline did not start with NUCLEOS_LIFELINE=1")
	}
	// A write on an io.Pipe returns only once it has been read, so this proves the watcher is
	// reading; a watcher still reading has not cut.
	wrote := make(chan error, 1)
	go func() {
		_, err := w.Write([]byte("still here"))
		wrote <- err
	}()
	select {
	case err := <-wrote:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("nothing is reading the lifeline")
	}
	select {
	case <-done:
		t.Fatal("cut while the line was still held")
	default:
	}
	w.Close() // the daemon dying, as the kernel reports it
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("still running five seconds after the line was cut")
	}
}

func TestABrokenLifelineAlsoShutsDown(t *testing.T) {
	r, w := io.Pipe()
	done := make(chan struct{})
	if !watchLifeline(askedFor, r, func() { close(done) }) {
		t.Fatal("watchLifeline did not start with NUCLEOS_LIFELINE=1")
	}
	w.CloseWithError(errors.New("the pipe broke")) // a read that fails rather than ends
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("still running five seconds after the line broke")
	}
}

func TestAnArmedLifelineIgnoresSIGPIPE(t *testing.T) {
	r, w := io.Pipe()
	defer w.Close()
	if !watchLifeline(askedFor, r, func() {}) {
		t.Fatal("watchLifeline did not start with NUCLEOS_LIFELINE=1")
	}
	// On Windows a broken pipe is a write error, never a signal, so there is nothing to observe.
	if runtime.GOOS != "windows" && !signal.Ignored(syscall.SIGPIPE) {
		t.Fatal("SIGPIPE is not ignored: the first log line after the daemon dies would kill the process before its shutdown ran")
	}
}
