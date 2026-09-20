package main

import (
	"bytes"
	"errors"
	"io"
	"os"
	"os/exec"
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

// sigpipeHelperVar turns TestAnArmedLifelineCatchesSIGPIPE into its own helper process: "armed" arms
// the lifeline first, "unarmed" does not, and both then write to a stdout nobody will ever read.
const sigpipeHelperVar = "NUCLEOS_LIFELINE_SIGPIPE_HELPER"

func TestAnArmedLifelineCatchesSIGPIPE(t *testing.T) {
	if mode := os.Getenv(sigpipeHelperVar); mode != "" {
		if mode == "armed" {
			held, _ := io.Pipe() // never closed: the daemon is still holding the line
			watchLifeline(askedFor, held, func() {})
		}
		// stdout's read end is already closed, as it is once the daemon is gone.
		os.Stdout.Write([]byte("logged after the daemon died\n"))
		os.Exit(0)
	}
	// On Windows a broken pipe is a write error, never a signal, so there is nothing to observe.
	if runtime.GOOS == "windows" {
		t.Skip("SIGPIPE is a Unix signal")
	}
	r, w := io.Pipe()
	defer w.Close()
	if !watchLifeline(askedFor, r, func() {}) {
		t.Fatal("watchLifeline did not start with NUCLEOS_LIFELINE=1")
	}

	// A program this sidecar starts gets the default disposition back. An inherited SIG_IGN lets a
	// transcriber keep printing into an output cap that has stopped reading.
	out, err := exec.Command("sh", "-c", "kill -s PIPE $$; echo survived").CombinedOutput()
	if !diedOfSIGPIPE(err) || bytes.Contains(out, []byte("survived")) {
		t.Errorf("a child of an armed sidecar was not killed by SIGPIPE (err %v, output %q): it inherited an ignored SIGPIPE", err, out)
	}

	// And this process is still not ended by a write to a broken stdout. The unarmed run is the
	// control: without it a helper that never reached its write would pass as well.
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	for _, mode := range []string{"unarmed", "armed"} {
		rd, wr, err := os.Pipe()
		if err != nil {
			t.Fatal(err)
		}
		rd.Close()
		helper := exec.Command(self, "-test.run=^TestAnArmedLifelineCatchesSIGPIPE$")
		helper.Env = append(os.Environ(), sigpipeHelperVar+"="+mode)
		helper.Stdout = wr
		var stderr bytes.Buffer
		helper.Stderr = &stderr
		err = helper.Run()
		wr.Close()
		switch {
		case mode == "unarmed" && !diedOfSIGPIPE(err):
			t.Fatalf("control: an unarmed helper was not killed by SIGPIPE (err %v, stderr %q), so this test observes nothing", err, stderr.Bytes())
		case mode == "armed" && err != nil:
			t.Fatalf("an armed helper did not survive a write to a broken stdout (err %v, stderr %q)", err, stderr.Bytes())
		}
	}
}

// diedOfSIGPIPE reports whether a finished command was killed by SIGPIPE.
func diedOfSIGPIPE(err error) bool {
	var exit *exec.ExitError
	if !errors.As(err, &exit) {
		return false
	}
	status, ok := exit.Sys().(syscall.WaitStatus)
	return ok && status.Signaled() && status.Signal() == syscall.SIGPIPE
}
