package launch

import (
	"os"
	"os/exec"
	"strconv"
	"syscall"
)

// devtoolsPipes are the four pipe ends of one DevTools connection.
type devtoolsPipes struct {
	browserRead, browserWrite *os.File
	parentWrite, parentRead   *os.File
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

// wire lists the browser's two handles as inherited and returns the switch naming them.
//
// Windows has no fd 3/4. Chromium (devtools_pipe_handler.cc, ReadFile/WriteFile on handles) takes
// --remote-debugging-pipe together with --remote-debugging-io-pipes=<read>,<write>: raw Win32 handle
// values in DECIMAL, the browser's read end first, then its write end. This is the spelling and order
// the Aspire BrowserLogsPipeBrowserProcessLauncher.Windows.cs launcher uses. The handles reach the
// child through SysProcAttr.AdditionalInheritedHandles; os.Pipe makes them inheritable.
// It must run after startSuspended, whose CreationFlags it preserves.
func (p *devtoolsPipes) wire(cmd *exec.Cmd) []string {
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	rd := syscall.Handle(p.browserRead.Fd())
	wr := syscall.Handle(p.browserWrite.Fd())
	cmd.SysProcAttr.AdditionalInheritedHandles = append(cmd.SysProcAttr.AdditionalInheritedHandles, rd, wr)
	return []string{"--remote-debugging-io-pipes=" + strconv.FormatUint(uint64(rd), 10) + "," + strconv.FormatUint(uint64(wr), 10)}
}
