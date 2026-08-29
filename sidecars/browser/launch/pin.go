// §spec pilar-de-browser

package launch

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"runtime"
	"time"
)

// The pin itself — spec §3.3, and the reason this file is short and boring on purpose.
//
// Every field below was measured rather than copied. The digest is the sha256 of the archive as it
// was actually served, taken by downloading it; a digest transcribed from a page somewhere would
// verify that the page and the archive agree, which is not the property this needs.
//
// # What a revision bump costs, and who owns it
//
// Changing `pinnedRevision` means re-downloading the archive, re-hashing it, and putting the new
// digest here. There is no way to do it correctly without fetching the bytes, and that is deliberate:
// the alternative is an unverified install of the one process that renders hostile HTML while holding
// the owner's logged-in profiles. Spec §13 records that the patching cadence still needs an owner —
// this file is where that owner's work lands, and `ErrNoDigest` is what stops anybody skipping it.

// pinnedRevision is the Chromium snapshot this installation owns.
const pinnedRevision = "1680269"

// snapshotBase is Chromium's own continuous-build bucket. Not a mirror and not a redirect: the
// digest below was taken from what this exact URL served, so a different host serving the same
// revision is a different fact.
const snapshotBase = "https://storage.googleapis.com/chromium-browser-snapshots"

// DefaultPin is the pinned Chromium for the platform this binary was built for.
//
// Only windows/amd64 carries a digest today, because that is the platform the digest was measured on
// and a digest cannot be guessed. The others come back with an empty Sha256 on purpose: `Download`
// refuses that with [ErrNoDigest], which is a refusal that names what is missing — strictly better
// than a pin that installs an unverified browser on a platform nobody has tested.
func DefaultPin() Pin {
	switch runtime.GOOS + "/" + runtime.GOARCH {
	case "windows/amd64":
		return Pin{
			Revision:            pinnedRevision,
			URL:                 snapshotBase + "/Win_x64/" + pinnedRevision + "/chrome-win.zip",
			Sha256:              "ef6c1ad450235f616e22d20ced338293a7f08abb9dda59ce9d534c7f4136f2f7",
			ExecutableInArchive: "chrome-win/chrome.exe",
		}
	case "linux/amd64":
		return Pin{
			Revision:            pinnedRevision,
			URL:                 snapshotBase + "/Linux_x64/" + pinnedRevision + "/chrome-linux.zip",
			ExecutableInArchive: "chrome-linux/chrome",
		}
	case "darwin/arm64":
		return Pin{
			Revision:            pinnedRevision,
			URL:                 snapshotBase + "/Mac_Arm/" + pinnedRevision + "/chrome-mac.zip",
			ExecutableInArchive: "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
		}
	default:
		// No URL either, so the failure is "this platform has no pinned browser" rather than a
		// download that 404s and reads like a network problem.
		return Pin{Revision: pinnedRevision}
	}
}

// ErrNoPinForPlatform means this build has no pinned Chromium at all.
var ErrNoPinForPlatform = fmt.Errorf("launch: no pinned chromium for %s/%s", runtime.GOOS, runtime.GOARCH)

// FetchTimeout bounds one attempt at the archive. Generous, because it is ~350MB over whatever
// connection the machine has, and a timeout shorter than a slow connection would turn "this takes a
// while" into a failure that retries forever and never finishes.
const FetchTimeout = 30 * time.Minute

// HTTPFetcher is the production [Fetcher]: it fetches over HTTPS and nothing else.
//
// The `Fetcher` seam exists so `Download` can be tested without the internet; this is the half that
// could not be. It is deliberately thin — no redirect policy of its own, no caching, no resume —
// because everything that decides whether the bytes are acceptable happens after it, in `Download`,
// against the pinned digest. A fetcher that was clever about partial downloads would be a fetcher
// that could hand `Download` a body it had assembled itself.
type HTTPFetcher struct {
	Client *http.Client
}

func (f HTTPFetcher) Fetch(ctx context.Context, url string) (io.ReadCloser, error) {
	client := f.Client
	if client == nil {
		client = &http.Client{Timeout: FetchTimeout}
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	response, err := client.Do(request)
	if err != nil {
		return nil, err
	}
	if response.StatusCode != http.StatusOK {
		response.Body.Close()
		return nil, fmt.Errorf("launch: %s answered %s", url, response.Status)
	}
	return response.Body, nil
}
