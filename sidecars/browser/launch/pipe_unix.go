//go:build !windows

package launch

import (
	"os"
	"os/exec"
)

// devtoolsPipes are the four pipe ends of one DevTools connection.
type devtoolsPipes struct {
	// browserRead/browserWrite are the browser's ends: it reads CDP from fd 3 and writes to fd 4.
	browserRead, browserWrite *os.File
	// parentWrite/parentRead are this process's ends.
	parentWrite, parentRead *os.File
}

func newDevtoolsPipes() (*devtoolsPipes, error) {
	browserRead, parentWrite, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	parentRead, browserWrite, err := os.Pipe()
	if err != nil {
		_ = browserRead.Close()
		_ = parentWrite.Close()
		return nil, err
	}
	return &devtoolsPipes{browserRead: browserRead, browserWrite: browserWrite, parentWrite: parentWrite, parentRead: parentRead}, nil
}

// wire hands the browser its ends as fds 3 and 4, which is where --remote-debugging-pipe looks
// (Chromium content/browser/devtools/devtools_pipe_handler.cc). No extra argument is needed.
// It must run after ownProcessGroup, which replaces SysProcAttr.
func (p *devtoolsPipes) wire(cmd *exec.Cmd) []string {
	cmd.ExtraFiles = []*os.File{p.browserRead, p.browserWrite}
	return nil
}
