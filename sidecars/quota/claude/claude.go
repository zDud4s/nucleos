// Package claude reads the owner's Claude quota from the vendor's own OAuth endpoint.
//
// This is the only code in NucleOS that opens a connection off this machine on the quota path, and
// it holds the owner's Claude token for the duration of one request and no longer. The token is
// never written to the database, never returned to the núcleo, and never logged — not at debug
// level, not inside an error. Errors returned from here are safe to put in a feed row.
//
// The token belongs to Claude Code, not to NucleOS. We read it where it lies and we do not manage
// it: an expired token is reported as Unmeasured and the owner re-authenticates in Claude Code.
// Refreshing it would mean writing another application's credential file (design D2).
package claude

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"time"

	"nucleosquota/reading"
)

// Endpoint is the vendor's usage endpoint — the same one Claude Code's own `/usage` reads, which is
// why the figure here never disagrees with what the CLI reports.
const Endpoint = "https://api.anthropic.com/api/oauth/usage"

// Name is how this provider is spelled everywhere above this package.
const Name = "claude"

// ErrNoToken is returned when there is no usable credential. Distinguished from a transport failure
// because the two mean different things to the owner: one is "log in again", the other is "the
// network or the vendor is having a bad day".
var ErrNoToken = errors.New("no usable Claude credential")

// credentials is the slice of ~/.claude/.credentials.json this package needs.
//
// Only three fields, out of a file that holds more: a reader that asks for less is a reader that
// keeps working when the other application changes its own file.
type credentials struct {
	OAuth struct {
		AccessToken string `json:"accessToken"`
		// ExpiresAt is epoch milliseconds, not seconds. Reading it as seconds puts expiry in 1970
		// and makes every token look dead.
		ExpiresAt     int64  `json:"expiresAt"`
		RateLimitTier string `json:"rateLimitTier"`
	} `json:"claudeAiOauth"`
}

// window is one of the two top-level windows in the response.
//
// Only the two fields that matter are declared. The rest of the payload — and on 2026-09-19 that
// was some sixteen code-named keys, nearly all null (`tangelo`, `iguana_necktie`, `nimbus_quill`,
// `cinder_cove`, …) — is ignored by construction. Those are experiment slots that appear and
// vanish without notice, so a struct that insisted on knowing them would break itself on the
// vendor's schedule.
type window struct {
	// Utilization is a PERCENTAGE in [0,100] carried in a float field, and the name is a trap: it
	// reads as a fraction and is not one. Measured against the live endpoint on 2026-09-19 it
	// answered 54.0 and 46.0 while `limits[].percent` in the same payload answered 54 and 46 — the
	// same unit, two spellings. Forwarding it untouched draws every ring at its cap.
	Utilization float64 `json:"utilization"`
	// ResetsAt may be absent. The 2026-09-19 capture had a populated window with a null reset.
	ResetsAt *time.Time `json:"resets_at"`
}

type usageResponse struct {
	FiveHour *window `json:"five_hour"`
	SevenDay *window `json:"seven_day"`
	// Limits carries the vendor's own severity wording. Recorded for display, never acted on.
	Limits []struct {
		Severity string `json:"severity"`
		IsActive bool   `json:"is_active"`
	} `json:"limits"`
}

// Reader fetches the quota. One per process; safe to reuse.
type Reader struct {
	client *http.Client
	// home is the directory holding `.claude`. A field rather than a call to os.UserHomeDir so the
	// tests can point it somewhere with a fixture in it.
	home string
	// endpoint is Endpoint in every build. A field only so the tests can aim this at an httptest
	// server: the alternative is a suite that either reaches the real vendor or never exercises the
	// parser at all, and the parser is where the interesting mistakes live.
	endpoint string
}

// New builds a Reader bounded by timeout.
func New(timeout time.Duration, home string) *Reader {
	return &Reader{client: &http.Client{Timeout: timeout}, home: home, endpoint: Endpoint}
}

// Read returns this provider's current quota, or an Unmeasured reading explaining why not.
//
// It does not return an error for the ordinary failures — no token, expired token, endpoint
// refusing — because those are answers about the provider rather than faults of this process, and
// the caller draws them as a dashed ring either way. A returned error means the request itself
// could not be made.
func (r *Reader) Read(ctx context.Context, now time.Time) reading.Provider {
	token, tier, err := r.credential(now)
	if err != nil {
		return reading.Unavailable(Name, err.Error(), now)
	}

	body, err := r.fetch(ctx, token)
	if err != nil {
		return reading.Unavailable(Name, err.Error(), now)
	}

	windows := make([]reading.Window, 0, 2)
	if body.FiveHour != nil {
		windows = append(windows, toWindow("5h", body.FiveHour))
	}
	if body.SevenDay != nil {
		windows = append(windows, toWindow("7d", body.SevenDay))
	}
	if len(windows) == 0 {
		// The call succeeded and neither window was there. That is the vendor changing the shape
		// under us, and it is the one case where reporting `official` would be a lie.
		return reading.Unavailable(
			Name,
			"the usage endpoint answered without five_hour or seven_day",
			now,
		)
	}

	p := reading.Provider{
		Name:     Name,
		Fidelity: reading.Official,
		ReadAt:   now,
		Windows:  reading.MarkStale(windows, now),
		Severity: severity(body),
	}
	if tier != "" {
		p.Detail = "tier " + tier
	}
	return p
}

func toWindow(name string, w *window) reading.Window {
	return reading.Window{Name: name, UsedFraction: clamp(w.Utilization / 100), ResetsAt: w.ResetsAt}
}

// clamp bounds a converted reading to [0,1].
//
// It deliberately does NOT rescue a value that is wildly out of range, because that is how this
// package shipped its first bug: `utilization` was assumed to be a fraction, 54.0 arrived, and the
// clamp quietly turned it into a full ring instead of letting an impossible number be noticed. The
// unit conversion above is the fix; this is only for the rounding at the edges. Anything an order
// of magnitude out should fail a test, not be smoothed over here.
func clamp(f float64) float64 {
	if f < 0 {
		return 0
	}
	if f > 1 {
		return 1
	}
	return f
}

// severity reports the worst active severity the vendor named, or "" when it named none.
func severity(body *usageResponse) string {
	for _, l := range body.Limits {
		if l.IsActive && l.Severity != "" {
			return l.Severity
		}
	}
	return ""
}

// credential reads the token, refusing rather than returning one already expired.
//
// Refusing early matters: an expired token gets a 401, and a 401 from this endpoint is
// indistinguishable at the call site from the vendor revoking access. Checking the clock first
// turns a confusing error into "log in again".
func (r *Reader) credential(now time.Time) (token, tier string, err error) {
	path := filepath.Join(r.home, ".claude", ".credentials.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return "", "", fmt.Errorf("%w: Claude Code has not signed in on this machine", ErrNoToken)
		}
		// Deliberately not wrapping the OS error: its text can carry the full path, and the path is
		// the one thing in this package worth not repeating into a feed row.
		return "", "", fmt.Errorf("%w: the credential file could not be read", ErrNoToken)
	}

	var creds credentials
	if err := json.Unmarshal(raw, &creds); err != nil {
		return "", "", fmt.Errorf("%w: the credential file is not the JSON we expect", ErrNoToken)
	}
	if creds.OAuth.AccessToken == "" {
		return "", "", fmt.Errorf("%w: the credential file carries no access token", ErrNoToken)
	}
	if creds.OAuth.ExpiresAt > 0 {
		expiry := time.UnixMilli(creds.OAuth.ExpiresAt)
		if !expiry.After(now) {
			return "", "", fmt.Errorf(
				"%w: it expired %s ago — sign in again in Claude Code",
				ErrNoToken, now.Sub(expiry).Round(time.Minute),
			)
		}
	}
	return creds.OAuth.AccessToken, creds.OAuth.RateLimitTier, nil
}

func (r *Reader) fetch(ctx context.Context, token string) (*usageResponse, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, r.endpoint, nil)
	if err != nil {
		return nil, fmt.Errorf("the usage request could not be built")
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")

	resp, err := r.client.Do(req)
	if err != nil {
		// net/http puts the URL in its error, never the header, so this is safe to surface. The
		// message is kept short because it ends up in front of the owner.
		return nil, fmt.Errorf("the usage endpoint could not be reached")
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
			return nil, fmt.Errorf("the usage endpoint refused the credential (%d)", resp.StatusCode)
		}
		return nil, fmt.Errorf("the usage endpoint answered %d", resp.StatusCode)
	}

	var body usageResponse
	// A ceiling on a response that should be a few kilobytes. The vendor has no reason to send more
	// and this process has no reason to hold more.
	if err := json.NewDecoder(http.MaxBytesReader(nil, resp.Body, 1<<20)).Decode(&body); err != nil {
		return nil, fmt.Errorf("the usage endpoint answered something that is not the JSON we expect")
	}
	return &body, nil
}
