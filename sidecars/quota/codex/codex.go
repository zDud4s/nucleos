// Package codex reads the owner's Codex quota from the rollout files Codex leaves on this machine.
//
// No network. Codex writes its own rate-limit readings into every session transcript, so the
// freshest one on disk is the best available answer — which is exactly why this provider is
// `derived` and not `official`: it is true as of the last time the owner ran something, and says
// nothing about the minutes since.
//
// The shape read here is an undocumented de facto contract (design R5). Everything below is written
// to degrade to Unmeasured rather than to a wrong number when that contract changes.
package codex

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"time"

	"nucleosquota/reading"
)

// Name is how this provider is spelled everywhere above this package.
const Name = "codex"

// tailBytes bounds how much of a transcript is read looking for the last reading. A session file
// grows without limit; the readings are appended, so the answer is always near the end. The same
// trick, and roughly the same size, as `scripts/statusline-context.py`.
const tailBytes = 256 << 10

// maxDaysBack is how far the walk goes before giving up. Codex partitions sessions by date, so this
// is a bound on directories, not on files: a machine idle for a fortnight reports Unmeasured rather
// than spending the poll interval walking a year of history.
const maxDaysBack = 30

// limits is the slice of the rollout's `rate_limits` object this package needs.
type limits struct {
	Primary   *bucket `json:"primary"`
	Secondary *bucket `json:"secondary"`
	PlanType  string  `json:"plan_type"`
}

type bucket struct {
	// UsedPercent is a PERCENTAGE despite being a float: the 2026-09-19 capture carried 1.0 and 6.0
	// meaning one and six per cent. The Anthropic endpoint's `utilization` in the same design is a
	// fraction. Dividing here, once, is the whole reason package reading exists.
	UsedPercent float64 `json:"used_percent"`
	// WindowMinutes is how the window is named: 300 is the five-hour window, 10080 the seven-day
	// one (the latter matching WEEKLY_WINDOW_MINUTES in `scripts/usage_split.py`).
	WindowMinutes int `json:"window_minutes"`
	// ResetsAt is epoch SECONDS here, where Anthropic sends an ISO string. Two providers, two
	// spellings of an instant; both become a time.Time before leaving their package.
	ResetsAt int64 `json:"resets_at"`
}

// Reader finds and parses the freshest rollout.
type Reader struct {
	// sessions is the directory holding Codex's date-partitioned sessions. A field so tests can
	// point it at a fixture.
	sessions string
}

// New builds a Reader over ~/.codex/sessions.
func New(home string) *Reader {
	return &Reader{sessions: filepath.Join(home, ".codex", "sessions")}
}

// Read returns this provider's current quota, or an Unmeasured reading explaining why not.
func (r *Reader) Read(now time.Time) reading.Provider {
	path, err := r.freshestRollout(now)
	if err != nil {
		return reading.Unavailable(Name, err.Error(), now)
	}

	found, err := lastLimits(path)
	if err != nil {
		return reading.Unavailable(Name, err.Error(), now)
	}

	windows := make([]reading.Window, 0, 2)
	for _, b := range []*bucket{found.Primary, found.Secondary} {
		if b == nil || b.WindowMinutes == 0 {
			continue
		}
		name := windowName(b.WindowMinutes)
		if name == "" {
			// A window this design has no ring for. Skipped rather than guessed at: the notch draws
			// 5h and 7d, and inventing a third would put an unlabelled ring on screen.
			continue
		}
		var resets *time.Time
		if b.ResetsAt > 0 {
			t := time.Unix(b.ResetsAt, 0).UTC()
			resets = &t
		}
		windows = append(windows, reading.Window{
			Name:         name,
			UsedFraction: clamp(b.UsedPercent / 100),
			ResetsAt:     resets,
		})
	}

	if len(windows) == 0 {
		return reading.Unavailable(Name, "the newest rollout carries no window this notch draws", now)
	}

	p := reading.Provider{
		Name:     Name,
		Fidelity: reading.Derived,
		ReadAt:   now,
		Windows:  reading.MarkStale(windows, now),
	}
	if found.PlanType != "" {
		p.Detail = "plan " + found.PlanType
	}
	return p
}

// windowName maps Codex's minutes to the design's vocabulary. Exact matches only — a window of 301
// minutes is not the five-hour window with rounding, it is a contract that changed.
func windowName(minutes int) string {
	switch minutes {
	case 300:
		return "5h"
	case 10080:
		return "7d"
	default:
		return ""
	}
}

func clamp(f float64) float64 {
	if f < 0 {
		return 0
	}
	if f > 1 {
		return 1
	}
	return f
}

// freshestRollout walks the date-partitioned tree backwards from today and returns the most
// recently modified transcript it finds.
//
// Walking by date rather than globbing the whole tree keeps this cheap on a machine with years of
// sessions: the answer is nearly always in today's or yesterday's directory, and the loop stops at
// the first day that has one.
func (r *Reader) freshestRollout(now time.Time) (string, error) {
	if _, err := os.Stat(r.sessions); err != nil {
		return "", fmt.Errorf("Codex has left no sessions on this machine")
	}
	for back := 0; back < maxDaysBack; back++ {
		day := now.AddDate(0, 0, -back)
		dir := filepath.Join(r.sessions, day.Format("2006"), day.Format("01"), day.Format("02"))
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}
		newest, newestAt := "", time.Time{}
		for _, e := range entries {
			if e.IsDir() || filepath.Ext(e.Name()) != ".jsonl" {
				continue
			}
			info, err := e.Info()
			if err != nil {
				continue
			}
			if info.ModTime().After(newestAt) {
				newest, newestAt = filepath.Join(dir, e.Name()), info.ModTime()
			}
		}
		if newest != "" {
			return newest, nil
		}
	}
	return "", fmt.Errorf("no Codex session in the last %d days", maxDaysBack)
}

// lastLimits returns the final rate_limits object in a transcript.
//
// The tail is read rather than the file, and the first complete line is dropped because a tail
// almost certainly begins mid-line — parsing half a JSON object would be a decode error reported as
// if the contract had changed.
func lastLimits(path string) (*limits, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("the newest Codex session could not be read")
	}
	defer f.Close()

	info, err := f.Stat()
	if err != nil {
		return nil, fmt.Errorf("the newest Codex session could not be measured")
	}
	start := int64(0)
	if info.Size() > tailBytes {
		start = info.Size() - tailBytes
	}
	if _, err := f.Seek(start, io.SeekStart); err != nil {
		return nil, fmt.Errorf("the newest Codex session could not be seeked")
	}

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 64<<10), 8<<20)
	if start > 0 {
		scanner.Scan() // partial line
	}

	var found *limits
	for scanner.Scan() {
		line := scanner.Bytes()
		if !bytes.Contains(line, []byte(`"rate_limits"`)) {
			continue
		}
		var doc map[string]json.RawMessage
		if err := json.Unmarshal(line, &doc); err != nil {
			continue
		}
		if raw := search(doc); raw != nil {
			var parsed limits
			if err := json.Unmarshal(raw, &parsed); err == nil {
				found = &parsed
			}
		}
	}
	if found == nil {
		return nil, fmt.Errorf("the newest Codex session carries no rate-limit reading yet")
	}
	return found, nil
}

// search finds `rate_limits` at any depth. Codex has moved it before; looking for the key rather
// than for a path means a move costs nothing here.
func search(doc map[string]json.RawMessage) json.RawMessage {
	if raw, ok := doc["rate_limits"]; ok {
		return raw
	}
	keys := make([]string, 0, len(doc))
	for k := range doc {
		keys = append(keys, k)
	}
	sort.Strings(keys) // deterministic, so a malformed transcript fails the same way twice
	for _, k := range keys {
		var nested map[string]json.RawMessage
		if err := json.Unmarshal(doc[k], &nested); err != nil {
			continue
		}
		if raw := search(nested); raw != nil {
			return raw
		}
	}
	return nil
}
