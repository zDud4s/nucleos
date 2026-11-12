// §spec pilar-de-browser

package launch

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

const jobHelperVar = "GO_WANT_JOB_HELPER"

// TestJobHelperProcess stands in for Chromium. As "parent" it spawns a grandchild the ordinary way,
// the way Chromium spawns its helpers, reports the grandchild's pid and sleeps; as "grandchild" it
// only sleeps. Neither returns before the test kills it.
func TestJobHelperProcess(t *testing.T) {
	switch os.Getenv(jobHelperVar) {
	case "parent":
		grandchild := exec.Command(os.Args[0], "-test.run=^TestJobHelperProcess$")
		grandchild.Env = append(os.Environ(), jobHelperVar+"=grandchild")
		if err := grandchild.Start(); err != nil {
			fmt.Println("error=" + err.Error())
			os.Exit(1)
		}
		fmt.Printf("grandchild=%d\n", grandchild.Process.Pid)
		time.Sleep(time.Minute)
	case "grandchild":
		time.Sleep(time.Minute)
	}
}

// TestClosingTheJobReapsAGrandchild is the case Stop cannot cover: the sidecar dies, the kernel
// closes its job handle, and every process in the job has to go with it — including one Chromium
// spawned after it was resumed, which only the job's inheritance puts there.
func TestClosingTheJobReapsAGrandchild(t *testing.T) {
	job, err := newKillOnCloseJob()
	if err != nil {
		t.Fatal(err)
	}
	jobOpen := true
	defer func() {
		if jobOpen {
			_ = syscall.CloseHandle(job)
		}
	}()

	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	cmd := exec.Command(os.Args[0], "-test.run=^TestJobHelperProcess$")
	cmd.Env = append(os.Environ(), jobHelperVar+"=parent")
	cmd.Stdout = w
	startSuspended(cmd)
	if err := cmd.Start(); err != nil {
		w.Close()
		t.Fatal(err)
	}
	w.Close()
	// One pid, never a tree: a regressed cleanup must not reach anything that is not this test's.
	defer func() {
		_ = cmd.Process.Kill()
		_, _ = cmd.Process.Wait()
	}()
	if err := assignAndResume(job, cmd); err != nil {
		t.Fatal(err)
	}

	reported := make(chan string, 1)
	go func() {
		lines := bufio.NewScanner(r)
		for lines.Scan() {
			if line := strings.TrimSpace(lines.Text()); strings.HasPrefix(line, "grandchild=") || strings.HasPrefix(line, "error=") {
				reported <- line
				return
			}
		}
		reported <- ""
	}()
	var line string
	select {
	case line = <-reported:
	case <-time.After(10 * time.Second):
		t.Fatal("the resumed stand-in never reported a grandchild: was it resumed at all?")
	}
	gpid, err := strconv.Atoi(strings.TrimPrefix(line, "grandchild="))
	if err != nil {
		t.Fatalf("the stand-in said %q, want grandchild=<pid>", line)
	}

	// A handle taken while the grandchild is alive pins its pid, so the wait below cannot be fooled
	// by Windows handing that pid to someone else.
	grandchild, err := syscall.OpenProcess(syscall.SYNCHRONIZE|syscall.PROCESS_TERMINATE, false, uint32(gpid))
	if err != nil {
		t.Fatalf("opening the grandchild (pid %d): %v", gpid, err)
	}
	defer func() {
		_ = syscall.TerminateProcess(grandchild, 1)
		_ = syscall.CloseHandle(grandchild)
	}()

	// What the kernel does for a sidecar that exits without calling Stop.
	jobOpen = false
	if err := syscall.CloseHandle(job); err != nil {
		t.Fatalf("closing the job: %v", err)
	}

	exited := make(chan struct{})
	go func() {
		_, _ = cmd.Process.Wait()
		close(exited)
	}()
	select {
	case <-exited:
	case <-time.After(10 * time.Second):
		t.Fatal("the child outlived its job's last handle")
	}
	event, err := syscall.WaitForSingleObject(grandchild, 10_000)
	if err != nil || event != syscall.WAIT_OBJECT_0 {
		t.Fatalf("the grandchild outlived its job's last handle (wait = %d, %v)", event, err)
	}
}
