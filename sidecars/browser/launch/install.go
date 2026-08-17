package launch

import (
	"archive/zip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
)

// Pin is the exact Chromium this installation owns.
//
// Spec §3.3 chose to pin our own build rather than drive whatever browser the person happens to
// have: a Chrome that updates underneath us changes the fence's behaviour without anyone deciding
// to. The cost is ~250-300MB once per installation, and a patching cadence that still needs an
// owner (spec §13).
type Pin struct {
	// Revision is the Chromium snapshot number, and names the directory on disk.
	Revision string
	// URL is where the archive comes from.
	URL string
	// Sha256 is the hex digest the archive must have. Empty means unverified, which Download
	// refuses — see ErrNoDigest.
	Sha256 string
	// ExecutableInArchive is the path of chrome.exe inside the zip.
	ExecutableInArchive string
}

// ErrNoDigest refuses an unverified install.
//
// This archive becomes the process that renders hostile HTML while holding the owner's logged-in
// profiles. Fetching it over TLS says who served it, not what it is; without a pinned digest a
// compromised mirror or a redirected URL installs whatever it likes, once, permanently.
var ErrNoDigest = errors.New("launch: refusing to install a Chromium with no pinned sha256")

// ErrDigestMismatch means the bytes are not the bytes we pinned.
var ErrDigestMismatch = errors.New("launch: chromium archive does not match its pinned sha256")

// Install is the on-disk layout of spec §5.6.
type Install struct {
	// Root is %LOCALAPPDATA%\NucleOS\browser on Windows.
	Root string
	Pin  Pin
}

// Dir is where this revision lives. Revision-scoped, so a version bump is a new directory rather
// than an in-place overwrite of the binary a running session is executing.
func (i Install) Dir() string {
	return filepath.Join(i.Root, "chromium-"+i.Pin.Revision)
}

// ExecutablePath is the browser binary.
func (i Install) ExecutablePath() string {
	return filepath.Join(i.Dir(), filepath.FromSlash(i.Pin.ExecutableInArchive))
}

// ProfilesDir holds both kinds of profile (spec §5.6). It is a sibling of the Chromium directory and
// never a parent of it, which is what lets `profile.Store` delete inside it without being able to
// reach the browser binary.
//
// What lives in there, and under which name, is `profile`'s business and not this package's. An
// earlier version had ProjectProfile and EphemeralProfile here, building a path by concatenating an
// ID that nobody had validated — two path builders, one of them unguarded, for one layout.
func (i Install) ProfilesDir() string { return filepath.Join(i.Root, "profiles") }

// Present reports whether the pinned Chromium is installed and executable.
func (i Install) Present() bool {
	info, err := os.Stat(i.ExecutablePath())
	return err == nil && !info.IsDir() && info.Size() > 0
}

// Fetcher opens the archive. An interface so the download is testable without the internet — the
// same reason `search.Provider` exists in the web sidecar.
type Fetcher interface {
	Fetch(ctx context.Context, url string) (io.ReadCloser, error)
}

// Download fetches, verifies and extracts the pinned Chromium. It is a no-op if already present.
//
// Order matters: the whole archive is read and hashed BEFORE anything is extracted. Streaming
// straight to disk and checking the digest afterwards would leave a half-written, unverified
// browser on disk if the check failed, and something would eventually run it.
func (i Install) Download(ctx context.Context, fetcher Fetcher) error {
	if i.Present() {
		return nil
	}
	if i.Pin.Sha256 == "" {
		return ErrNoDigest
	}

	body, err := fetcher.Fetch(ctx, i.Pin.URL)
	if err != nil {
		return fmt.Errorf("fetching chromium: %w", err)
	}
	defer body.Close()

	archive, err := io.ReadAll(body)
	if err != nil {
		return fmt.Errorf("reading chromium archive: %w", err)
	}

	sum := sha256.Sum256(archive)
	if !strings.EqualFold(hex.EncodeToString(sum[:]), i.Pin.Sha256) {
		return fmt.Errorf("%w: got %s, want %s", ErrDigestMismatch, hex.EncodeToString(sum[:]), i.Pin.Sha256)
	}

	return i.extract(archive)
}

func (i Install) extract(archive []byte) error {
	reader, err := zip.NewReader(newByteReaderAt(archive), int64(len(archive)))
	if err != nil {
		return fmt.Errorf("opening chromium archive: %w", err)
	}
	dir := i.Dir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return fmt.Errorf("creating %s: %w", dir, err)
	}
	for _, entry := range reader.File {
		target, err := safeJoin(dir, entry.Name)
		if err != nil {
			return err
		}
		if entry.FileInfo().IsDir() {
			if err := os.MkdirAll(target, 0o755); err != nil {
				return err
			}
			continue
		}
		if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
			return err
		}
		if err := writeEntry(entry, target); err != nil {
			return err
		}
	}
	return nil
}

func writeEntry(entry *zip.File, target string) error {
	source, err := entry.Open()
	if err != nil {
		return err
	}
	defer source.Close()
	// 0o755 because the thing being unpacked is an executable and its libraries.
	out, err := os.OpenFile(target, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o755)
	if err != nil {
		return err
	}
	defer out.Close()
	// Bounded copy: a zip that claims a small compressed size and expands without end would
	// otherwise fill the disk before anyone noticed. 4GiB is far above any Chromium file and far
	// below a problem.
	const maxEntry = 4 << 30
	written, err := io.Copy(out, io.LimitReader(source, maxEntry))
	if err != nil {
		return err
	}
	if written == maxEntry {
		return fmt.Errorf("launch: archive entry %s is implausibly large", entry.Name)
	}
	return nil
}

// safeJoin refuses a zip entry that would escape the install directory.
//
// "../../../AppData/Roaming/..." in an entry name is the oldest trick there is, and this archive is
// the one thing we unpack from off the machine. The digest check above makes it unlikely; this
// makes it impossible, and the two are cheap enough to both have.
func safeJoin(root, name string) (string, error) {
	cleaned := filepath.Clean(filepath.FromSlash(name))
	if filepath.IsAbs(cleaned) || strings.HasPrefix(cleaned, "..") {
		return "", fmt.Errorf("launch: archive entry %q escapes the install directory", name)
	}
	target := filepath.Join(root, cleaned)
	// Belt and braces: compare the resolved path, not just the textual prefix.
	relative, err := filepath.Rel(root, target)
	if err != nil || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("launch: archive entry %q escapes the install directory", name)
	}
	return target, nil
}

// byteReaderAt adapts a []byte to io.ReaderAt without a temp file — the archive is already in
// memory because it had to be hashed whole before anything was written.
type byteReaderAt struct{ data []byte }

func newByteReaderAt(data []byte) *byteReaderAt { return &byteReaderAt{data: data} }

func (b *byteReaderAt) ReadAt(p []byte, off int64) (int, error) {
	if off < 0 || off >= int64(len(b.data)) {
		return 0, io.EOF
	}
	n := copy(p, b.data[off:])
	if n < len(p) {
		return n, io.EOF
	}
	return n, nil
}
