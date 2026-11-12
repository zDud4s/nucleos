// Package gemini reads the owner's Gemini subscription quota (Google AI Pro / Ultra) from the
// Code Assist endpoint that Gemini CLI and Antigravity signs in against.
//
// Same rules as package claude, and for the same reasons: the token belongs to Gemini CLI, it is
// read where it lies for the length of one request, and it is never written, returned or logged.
// An expired token is reported as Unmeasured and the owner refreshes it by running `gemini` — we do
// not refresh it ourselves, because that means holding Google's OAuth client and, sooner or later,
// writing another application's credential file (design D2).
//
// **Absent is not the same as unmeasured here, and that is the one rule this package adds.** Claude
// is the assistant NucleOS runs, so a missing Claude credential is worth a dashed ring. Most owners
// have never signed in to Gemini at all, and a dashed Gemini ring on every one of their screens
// would be noise about a product they do not use. So: no credential file, or an account the
// endpoint says has no subscription, and this provider is left out of the answer altogether.
//
// The endpoint is undocumented. What is known of it (2026-09/10, from the third-party readers that
// track it): `retrieveUserQuotaSummary` answers `buckets` — `gemini-5h` and `gemini-weekly`, each
// with a `remainingFraction` in [0,1] and an RFC 3339 `resetTime` — sometimes nested under
// `groups[].buckets`. The legacy `cloudcode-pa` host has been seen answering a stale 5-hour figure;
// `daily-cloudcode-pa` is the host the vendor's own IDE reads, so that is the one asked.
package gemini

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"nucleosquota/reading"
)

// Name is how this provider is spelled everywhere above this package.
const Name = "gemini"

const (
	// LoadEndpoint answers which Cloud project the account's Code Assist quota lives in, and its tier.
	LoadEndpoint = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist"
	// QuotaEndpoint is the summary the vendor's own `/usage` reads.
	QuotaEndpoint = "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary"
)

// The vendor's bucket ids, and the design's window names they become.
var buckets = map[string]string{
	"gemini-5h":     "5h",
	"gemini-weekly": "7d",
}

// errAbsent means there is nothing to report: Gemini was never signed in on this machine, or the
// account has no subscription. Never shown; the provider is simply left out.
var errAbsent = errors.New("no Gemini subscription on this machine")

// credentials is the slice of ~/.gemini/oauth_creds.json this package needs.
type credentials struct {
	AccessToken string `json:"access_token"`
	// ExpiryDate is epoch MILLISECONDS, like Claude's `expiresAt`.
	ExpiryDate int64 `json:"expiry_date"`
}

type bucket struct {
	BucketID string `json:"bucketId"`
	// RemainingFraction is what is LEFT, in [0,1]. Inverted at the edge: nothing past this package
	// sees anything but the used fraction. A pointer, because a missing field is not zero left.
	RemainingFraction *float64   `json:"remainingFraction"`
	ResetTime         *time.Time `json:"resetTime"`
	Disabled          bool       `json:"disabled"`
}

type summaryResponse struct {
	Buckets []bucket `json:"buckets"`
	Groups  []struct {
		Buckets []bucket `json:"buckets"`
	} `json:"groups"`
}

type loadResponse struct {
	// A plain id in the captures seen so far; an object with an `id` in some clients. Decoded
	// by hand (projectID) rather than trusting either shape.
	Project     json.RawMessage `json:"cloudaicompanionProject"`
	CurrentTier struct {
		ID string `json:"id"`
	} `json:"currentTier"`
}

// Reader fetches the quota. One per process. Not safe for concurrent Read calls: `serve` reads under
// its cache lock, so it never makes one.
type Reader struct {
	client *http.Client
	home   string
	// Fields only so the tests can aim this at an httptest server.
	loadEndpoint  string
	quotaEndpoint string
	// getenv is os.Getenv in every build; a field so the tests do not depend on the machine's own.
	getenv func(string) string
	// The project and tier, once asked. They do not change while the account does not, and asking
	// on every poll would double the calls this process makes to an endpoint it does not own.
	project string
	tier    string
	// token is the access token the project was looked up with: a new sign-in may be a new account,
	// so a different token asks again.
	token string
}

// New builds a Reader bounded by timeout.
func New(timeout time.Duration, home string) *Reader {
	return &Reader{
		client:        &http.Client{Timeout: timeout},
		home:          home,
		loadEndpoint:  LoadEndpoint,
		quotaEndpoint: QuotaEndpoint,
		getenv:        os.Getenv,
	}
}

// Read returns this provider's quota, and false when there is no Gemini subscription to report on.
func (r *Reader) Read(ctx context.Context, now time.Time) (reading.Provider, bool) {
	p, err := r.read(ctx, now)
	if errors.Is(err, errAbsent) {
		return reading.Provider{}, false
	}
	if err != nil {
		return reading.Unavailable(Name, err.Error(), now), true
	}
	return p, true
}

func (r *Reader) read(ctx context.Context, now time.Time) (reading.Provider, error) {
	token, err := r.credential(now)
	if err != nil {
		return reading.Provider{}, err
	}
	if r.project == "" || r.token != token {
		if err := r.load(ctx, token); err != nil {
			return reading.Provider{}, err
		}
		r.token = token
	}

	var body summaryResponse
	if err := r.post(ctx, r.quotaEndpoint, token, map[string]string{"project": r.project}, &body); err != nil {
		return reading.Provider{}, err
	}

	all := body.Buckets
	for _, group := range body.Groups {
		all = append(all, group.Buckets...)
	}
	found := map[string]reading.Window{}
	for _, b := range all {
		name, ok := buckets[b.BucketID]
		if !ok || b.Disabled || b.RemainingFraction == nil {
			continue
		}
		if _, seen := found[name]; seen {
			continue
		}
		found[name] = reading.Window{Name: name, UsedFraction: clamp(1 - *b.RemainingFraction), ResetsAt: b.ResetTime}
	}
	windows := make([]reading.Window, 0, 2)
	for _, name := range []string{"5h", "7d"} {
		if w, ok := found[name]; ok {
			windows = append(windows, w)
		}
	}
	if len(windows) == 0 {
		// The call succeeded and neither bucket was there: the vendor changed the shape, and
		// reporting `official` would be a lie.
		return reading.Provider{}, errors.New("the usage endpoint answered without gemini-5h or gemini-weekly")
	}

	p := reading.Provider{
		Name:     Name,
		Fidelity: reading.Official,
		ReadAt:   now,
		Windows:  reading.MarkStale(windows, now),
	}
	if r.tier != "" {
		p.Detail = "tier " + r.tier
	}
	return p, nil
}

// load asks which project the quota lives in. An explicit GOOGLE_CLOUD_PROJECT wins, as it does
// in Gemini CLI, and is sent along so a Workspace account is answered for the right project.
func (r *Reader) load(ctx context.Context, token string) error {
	explicit := r.getenv("GOOGLE_CLOUD_PROJECT")
	if explicit == "" {
		explicit = r.getenv("GOOGLE_CLOUD_PROJECT_ID")
	}
	request := map[string]any{
		"metadata": map[string]string{
			"ideType":    "IDE_UNSPECIFIED",
			"platform":   "PLATFORM_UNSPECIFIED",
			"pluginType": "GEMINI",
		},
	}
	if explicit != "" {
		request["cloudaicompanionProject"] = explicit
	}
	var body loadResponse
	if err := r.post(ctx, r.loadEndpoint, token, request, &body); err != nil {
		return err
	}
	project := projectID(body.Project)
	if project == "" {
		project = explicit
	}
	if project == "" {
		// Signed in, and no Code Assist project: a free personal account that has never been
		// onboarded. Nothing to measure, and nothing the owner is paying for.
		return errAbsent
	}
	r.project, r.tier = project, body.CurrentTier.ID
	return nil
}

func projectID(raw json.RawMessage) string {
	var id string
	if json.Unmarshal(raw, &id) == nil {
		return id
	}
	var object struct {
		ID string `json:"id"`
	}
	if json.Unmarshal(raw, &object) == nil {
		return object.ID
	}
	return ""
}

func (r *Reader) credential(now time.Time) (string, error) {
	raw, err := os.ReadFile(filepath.Join(r.home, ".gemini", "oauth_creds.json"))
	if err != nil {
		if os.IsNotExist(err) {
			return "", errAbsent
		}
		// Not wrapping the OS error: its text can carry the full path.
		return "", errors.New("the Gemini credential file could not be read")
	}
	var creds credentials
	if err := json.Unmarshal(raw, &creds); err != nil {
		return "", errors.New("the Gemini credential file is not the JSON we expect")
	}
	if creds.AccessToken == "" {
		return "", errors.New("the Gemini credential file carries no access token")
	}
	if creds.ExpiryDate > 0 {
		expiry := time.UnixMilli(creds.ExpiryDate)
		if !expiry.After(now) {
			return "", fmt.Errorf(
				"the Gemini token expired %s ago — run gemini to refresh it",
				now.Sub(expiry).Round(time.Minute),
			)
		}
	}
	return creds.AccessToken, nil
}

func (r *Reader) post(ctx context.Context, endpoint, token string, request, into any) error {
	payload, err := json.Marshal(request)
	if err != nil {
		return errors.New("the usage request could not be built")
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(payload))
	if err != nil {
		return errors.New("the usage request could not be built")
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")

	resp, err := r.client.Do(req)
	if err != nil {
		return errors.New("the Gemini usage endpoint could not be reached")
	}
	defer resp.Body.Close()
	// A ceiling on a response that should be a few kilobytes.
	limited := io.LimitReader(resp.Body, 1<<20)

	if resp.StatusCode != http.StatusOK {
		// A free account is refused with SUBSCRIPTION_REQUIRED: that is not a fault, it is an owner
		// with nothing to measure. The body is read for that word only and never surfaced.
		text, _ := io.ReadAll(limited)
		if resp.StatusCode == http.StatusForbidden && strings.Contains(string(text), "SUBSCRIPTION_REQUIRED") {
			return errAbsent
		}
		if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
			return fmt.Errorf("the Gemini usage endpoint refused the credential (%d)", resp.StatusCode)
		}
		return fmt.Errorf("the Gemini usage endpoint answered %d", resp.StatusCode)
	}
	if err := json.NewDecoder(limited).Decode(into); err != nil {
		return errors.New("the Gemini usage endpoint answered something that is not the JSON we expect")
	}
	return nil
}

// clamp bounds the inverted fraction to [0,1]; the vendor's own value is already a fraction.
func clamp(f float64) float64 {
	if f < 0 {
		return 0
	}
	if f > 1 {
		return 1
	}
	return f
}
