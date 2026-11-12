//go:build windows

package launch

import (
	"os"
	"strconv"
	"strings"
)

// fakePipeEnds parses --remote-debugging-io-pipes=<read>,<write> from the command line and wraps the
// two inherited raw handles.
func fakePipeEnds() (*os.File, *os.File) {
	const flag = "--remote-debugging-io-pipes="
	for _, arg := range os.Args {
		value, ok := strings.CutPrefix(arg, flag)
		if !ok {
			continue
		}
		parts := strings.Split(value, ",")
		if len(parts) != 2 {
			return nil, nil
		}
		read, err1 := strconv.ParseUint(parts[0], 10, 64)
		write, err2 := strconv.ParseUint(parts[1], 10, 64)
		if err1 != nil || err2 != nil {
			return nil, nil
		}
		return os.NewFile(uintptr(read), "pipe"), os.NewFile(uintptr(write), "pipe")
	}
	return nil, nil
}
