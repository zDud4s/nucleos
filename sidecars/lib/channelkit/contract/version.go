// Package contract holds the channel adapter contract: its version and the
// compatibility rule between an adapter and the core.
package contract

import "strconv"

// Version is the contract version this kit implements.
const Version = "1.0.0"

// Compatible reports whether an adapter built against adapterVersion can talk
// to a core implementing Version: same MAJOR, and the adapter's MINOR must not
// exceed the core's. PATCH is ignored. Anything that is not three
// dot-separated runs of digits is incompatible.
func Compatible(adapterVersion string) bool {
	a, ok := parseSemver(adapterVersion)
	if !ok {
		return false
	}
	c, ok := parseSemver(Version)
	if !ok {
		return false
	}
	return a[0] == c[0] && a[1] <= c[1]
}

// parseSemver parses "MAJOR.MINOR.PATCH" made of digits only.
func parseSemver(s string) ([3]int, bool) {
	var out [3]int
	part := 0
	digits := 0
	for i := 0; i <= len(s); i++ {
		if i == len(s) || s[i] == '.' {
			if digits == 0 || part > 2 {
				return out, false
			}
			v, err := strconv.Atoi(s[i-digits : i])
			if err != nil {
				return out, false
			}
			out[part] = v
			part++
			digits = 0
			continue
		}
		if s[i] < '0' || s[i] > '9' {
			return out, false
		}
		digits++
	}
	return out, part == 3
}
