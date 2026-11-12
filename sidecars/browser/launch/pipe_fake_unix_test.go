//go:build !windows

package launch

import "os"

// fakePipeEnds returns the fake browser's ends of the DevTools pipe: fd 3 is read, fd 4 is written.
func fakePipeEnds() (*os.File, *os.File) {
	return os.NewFile(3, "devtools-in"), os.NewFile(4, "devtools-out")
}
