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
// trick, and roughly the same size, as `.ai/scripts/statusline-context.py`.
const tailBytes = 256 << 10

// defaultScanBufferCap and defaultMaxLineBytes bound the scanner used to read a transcript's tail:
// the initial buffer and the point past which a single line is treated as unreadable rather than
// buffered further. These live on Reader (below) rather than as package vars, so a test can build a
// Reader with its own smaller values and exercise the "scan failed" path cheaply — without a shared
// mutable that a future t.Parallel() test would race on.
const (
	defaultScanBufferCap = 64 << 10
	defaultMaxLineBytes  = 8 << 20
)

// maxDaysBack is how far the walk goes before giving up. Codex partitions sessions by date, so this
// is a bound on directories, not on files: a machine idle for a fortnight reports Unmeasured rather
// than spending the poll interval walking a year of history.
const maxDaysBack = 30

// candidateBudget bounds how many rollout files one read may open, across every day directory
// scanned. Codex's date-partitioned directories can hold hundreds of sessions, and a run of
// meta-only sessions (started but with no token_count yet) must not turn a single poll into hundreds
// of file opens.
const candidateBudget = 50

// candidateDaysToScan bounds how many non-empty day directories are gathered before the first
// mtime-ordered attempt. Listing a day directory is cheap — no file is opened — so scanning it costs
// nothing; what candidateBudget guards against is opening and parsing files. Limiting the *gather*
// to a few populated days, rather than every day up to maxDaysBack, keeps that gather itself bounded
// on a machine with years of history, on the reasonable bet that the freshest reading is almost
// always within the last day or two. freshestReading widens beyond this only if none of the
// gathered candidates carries a usable reading.
const candidateDaysToScan = 3

// limits is the slice of the rollout's `rate_limits` object this package needs.
type limits struct {
	Primary   *bucket `json:"primary"`
	Secondary *bucket `json:"secondary"`
	PlanType  string  `json:"plan_type"`
}

type bucket struct {
	// UsedPercent is a PERCENTAGE despite being a float: the 2026-09-19 capture carried 1.0 and 6.0
	// meaning one and six per cent. The Anthropic endpoint's `utilization` in the same design is
	// also a percentage despite its name reading like a fraction (see claude.go) — the same unit,
	// two spellings. Dividing here, once, is the whole reason package reading exists.
	UsedPercent float64 `json:"used_percent"`
	// WindowMinutes is how the window is named: 300 is the five-hour window, 10080 the seven-day
	// one (the latter matching WEEKLY_WINDOW_MINUTES in `.ai/scripts/usage_split.py`).
	WindowMinutes int `json:"window_minutes"`
	// ResetsAt is epoch SECONDS here, where Anthropic sends an ISO string. Two providers, two
	// spellings of an instant; both become a time.Time before leaving their package.
	ResetsAt int64 `json:"resets_at"`
	// ResetsInSeconds is how older Codex builds spelled the same fact: an offset from the moment
	// the line was written rather than an absolute instant. Only usable together with the line's
	// own timestamp, which is why it is resolved in Read rather than here. A float, not an int64:
	// UsedPercent in this same struct is already a float, so a build that spells this one with a
	// fraction (17999.5) is plausible, and an int64 field would fail to unmarshal the whole `limits`
	// object over it — discarding an otherwise good reading rather than losing half a second.
	ResetsInSeconds *float64 `json:"resets_in_seconds"`
}

// foundLimits is one rate_limits reading together with the timestamp of the rollout line it came
// from. The timestamp is only needed to resolve an old-style `resets_in_seconds`, which is relative
// to the event rather than absolute — see bucket.ResetsInSeconds.
type foundLimits struct {
	limits
	// lineTime is the zero value when the line carried no parseable `timestamp`. A relative reset
	// is skipped rather than guessed at in that case (see Read).
	lineTime time.Time
}

// Reader finds and parses the freshest rollout.
type Reader struct {
	// sessions is the directory holding Codex's date-partitioned sessions. A field so tests can
	// point it at a fixture.
	sessions string
	// scanBufferCap and maxLineBytes bound the scanner lastLimits uses on a transcript's tail — see
	// the defaults' comment. Fields rather than package vars so a test can build a Reader with its
	// own smaller values instead of mutating shared state.
	scanBufferCap int
	maxLineBytes  int
}

// New builds a Reader over ~/.codex/sessions.
func New(home string) *Reader {
	return &Reader{
		sessions:      filepath.Join(home, ".codex", "sessions"),
		scanBufferCap: defaultScanBufferCap,
		maxLineBytes:  defaultMaxLineBytes,
	}
}

// Read returns this provider's current quota, or an Unmeasured reading explaining why not.
func (r *Reader) Read(now time.Time) reading.Provider {
	found, err := r.freshestReading(now)
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
		switch {
		case b.ResetsAt > 0:
			t := time.Unix(b.ResetsAt, 0).UTC()
			resets = &t
		case b.ResetsInSeconds != nil && !found.lineTime.IsZero():
			// The older spelling is an offset from when the line was written, not from now: "now"
			// could be days after the last time Codex ran, and would put every such window's reset
			// in the past.
			t := found.lineTime.Add(time.Duration(*b.ResetsInSeconds * float64(time.Second))).UTC()
			resets = &t
		}
		// Anything else — resets_in_seconds present but no base time to add it to — is left nil
		// rather than guessed at, per the design's rule that an unmeasured fact is reported as
		// absent, never as a number nobody can justify.
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

// rolloutFile is one candidate transcript, kept with its mtime so candidates within a day can be
// tried newest first.
type rolloutFile struct {
	path    string
	modTime time.Time
}

// freshestReading returns the first rollout, mtime-newest first, that carries a usable rate-limit
// reading.
//
// The walk is mtime-major, not day-major: a session started on the 18th and still being appended
// today lives in that day's directory with today's mtime, while a session started and abandoned
// this morning lives in today's directory with an *older* mtime than the still-live one. Visiting
// today's directory first and stopping at its newest file, as this used to, would hand back the
// abandoned file's stale reading over the live one sitting a day back. Gathering candidates across
// days and sorting by mtime before opening any of them is the fix.
//
// A session that has only just started writes `session_meta` and nothing else — no token_count, no
// rate_limits — so even the mtime-newest candidate can turn out to carry nothing usable; every
// candidate is tried, newest first, until one does.
//
// The gather itself stays bounded: candidateDaysToScan caps how many non-empty day directories
// contribute candidates before the first attempt (listing a day is cheap; opening a file is not),
// and candidateBudget caps how many files this call may actually open. Only if none of that first
// batch yields a reading does the walk widen, one further day at a time in the original day-major
// order, until maxDaysBack or the budget is exhausted — a machine idle for a fortnight should not
// pay for an mtime sort across a month of history to learn that.
func (r *Reader) freshestReading(now time.Time) (*foundLimits, error) {
	if _, err := os.Stat(r.sessions); err != nil {
		return nil, fmt.Errorf("Codex has left no sessions on this machine")
	}

	opened := 0
	candidates := make([]rolloutFile, 0, candidateBudget)
	back := 0
	for nonEmptyDays := 0; back < maxDaysBack && nonEmptyDays < candidateDaysToScan; back++ {
		files := dayFiles(r.sessions, now.AddDate(0, 0, -back))
		if len(files) == 0 {
			continue
		}
		nonEmptyDays++
		candidates = append(candidates, files...)
	}
	sortRolloutFiles(candidates)

	if found, err := r.tryCandidates(candidates, &opened); found != nil || err != nil {
		return found, err
	}

	// Widen: the gathered days had no candidate that carried a usable reading. This is the rare
	// path — a long run of meta-only sessions, or several abandoned days in a row — so falling back
	// to the original day-major order here costs nothing in practice.
	for ; back < maxDaysBack; back++ {
		files := dayFiles(r.sessions, now.AddDate(0, 0, -back))
		if len(files) == 0 {
			continue
		}
		sortRolloutFiles(files)
		if found, err := r.tryCandidates(files, &opened); found != nil || err != nil {
			return found, err
		}
	}
	return nil, fmt.Errorf("no Codex session in the last %d days carries a rate-limit reading", maxDaysBack)
}

// dayFiles lists one date-partitioned day directory's rollout files, or nil if the directory does
// not exist or holds none. Listing is a single ReadDir, not a file open, so calling it on empty or
// missing days is cheap.
func dayFiles(sessionsDir string, day time.Time) []rolloutFile {
	dir := filepath.Join(sessionsDir, day.Format("2006"), day.Format("01"), day.Format("02"))
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	files := make([]rolloutFile, 0, len(entries))
	for _, e := range entries {
		if e.IsDir() || filepath.Ext(e.Name()) != ".jsonl" {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		files = append(files, rolloutFile{filepath.Join(dir, e.Name()), info.ModTime()})
	}
	return files
}

// sortRolloutFiles orders newest first, stably. sort.Slice is not stable, so two files sharing an
// mtime — plausible given filesystem timestamp resolution — would pick an arbitrary winner on every
// run. The tiebreak falls back to the filename: Codex rollout filenames embed the session's own
// timestamp, so the later name is the later session when mtimes tie.
func sortRolloutFiles(files []rolloutFile) {
	sort.SliceStable(files, func(i, j int) bool {
		if !files[i].modTime.Equal(files[j].modTime) {
			return files[i].modTime.After(files[j].modTime)
		}
		return files[i].path > files[j].path
	})
}

// tryCandidates opens files in the given order, newest first, until one carries a usable reading or
// the shared candidateBudget across the whole call is exhausted. opened is shared across multiple
// calls (the initial gather and any widening pass) so the budget bounds the read as a whole, not
// each batch separately.
func (r *Reader) tryCandidates(files []rolloutFile, opened *int) (*foundLimits, error) {
	for _, f := range files {
		if *opened >= candidateBudget {
			return nil, fmt.Errorf(
				"no rate-limit reading in the newest %d Codex sessions", candidateBudget,
			)
		}
		*opened++
		found, err := r.lastLimits(f.path)
		if err != nil {
			// This file has nothing usable — a meta-only session, an unreadable file, a scan
			// failure. Move on to the next candidate rather than giving up.
			continue
		}
		return found, nil
	}
	return nil, nil
}

// lastLimits returns the final rate_limits object in a transcript, together with the timestamp of
// the line it came from.
//
// The tail is read rather than the file, and the first complete line is dropped because a tail
// almost certainly begins mid-line — parsing half a JSON object would be a decode error reported as
// if the contract had changed.
func (r *Reader) lastLimits(path string) (*foundLimits, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("this Codex session could not be read")
	}
	defer f.Close()

	info, err := f.Stat()
	if err != nil {
		return nil, fmt.Errorf("this Codex session could not be measured")
	}
	start := int64(0)
	if info.Size() > tailBytes {
		start = info.Size() - tailBytes
	}
	if _, err := f.Seek(start, io.SeekStart); err != nil {
		return nil, fmt.Errorf("this Codex session could not be seeked")
	}

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, r.scanBufferCap), r.maxLineBytes)
	if start > 0 {
		scanner.Scan() // partial line
	}

	var found *foundLimits
	for scanner.Scan() {
		line := scanner.Bytes()
		if !bytes.Contains(line, []byte(`"rate_limits"`)) {
			continue
		}
		var doc map[string]json.RawMessage
		if err := json.Unmarshal(line, &doc); err != nil {
			continue
		}
		raw := search(doc)
		if raw == nil {
			continue
		}
		var parsed limits
		if err := json.Unmarshal(raw, &parsed); err != nil {
			continue
		}
		// Skip empty exhausted-quota snapshots — a `rate_limits` object that parses but carries
		// neither bucket — so they do not clobber the last usable reading. The Python original
		// (usage.py) draws the same line: null and {} are valid JSON and worthless readings.
		if parsed.Primary == nil && parsed.Secondary == nil {
			continue
		}
		found = &foundLimits{limits: parsed, lineTime: lineTimestamp(doc)}
	}
	if err := scanner.Err(); err != nil {
		// In production this realistically fires only on a genuine I/O error: the scanner never
		// sees more than tailBytes (256 KiB) of the file, well under maxLineBytes (8 MiB), so "line
		// too long" cannot happen here outside a test that shrinks the buffer to exercise this path
		// cheaply. Wording the message around a scan failure rather than a specific cause, and
		// wrapping the underlying error, keeps it honest for whichever one actually fired. Silently
		// keeping whatever was found before the failure would risk reporting a reading that is no
		// longer the last one in the file, so the whole file is treated as unreadable and the
		// caller moves on to an older candidate instead.
		return nil, fmt.Errorf("this Codex session could not be fully scanned: %w", err)
	}
	if found == nil {
		return nil, fmt.Errorf("this Codex session carries no rate-limit reading yet")
	}
	return found, nil
}

// lineTimestamp reads the rollout envelope's own `timestamp`, when present and parseable. Used only
// to resolve the older `resets_in_seconds` spelling relative to the event that reported it; a line
// with no usable timestamp yields the zero Time, and Read skips that fallback rather than guessing.
func lineTimestamp(doc map[string]json.RawMessage) time.Time {
	raw, ok := doc["timestamp"]
	if !ok {
		return time.Time{}
	}
	var s string
	if err := json.Unmarshal(raw, &s); err != nil {
		return time.Time{}
	}
	t, err := time.Parse(time.RFC3339, s)
	if err != nil {
		return time.Time{}
	}
	return t
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
