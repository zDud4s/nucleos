// §spec pilar-de-browser

package launch

import (
	"archive/zip"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type fakeFetcher struct {
	body []byte
	err  error
	// URLs records what was asked for, so a test can assert the pinned url was the one used.
	URLs []string
}

func (f *fakeFetcher) Fetch(_ context.Context, url string) (io.ReadCloser, error) {
	f.URLs = append(f.URLs, url)
	if f.err != nil {
		return nil, f.err
	}
	return io.NopCloser(bytes.NewReader(f.body)), nil
}

// buildZip makes an archive with the given entries, and returns it with its digest.
func buildZip(t *testing.T, entries map[string]string) ([]byte, string) {
	t.Helper()
	var buf bytes.Buffer
	writer := zip.NewWriter(&buf)
	for name, content := range entries {
		file, err := writer.Create(name)
		if err != nil {
			t.Fatalf("create %s: %v", name, err)
		}
		if _, err := file.Write([]byte(content)); err != nil {
			t.Fatalf("write %s: %v", name, err)
		}
	}
	if err := writer.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	sum := sha256.Sum256(buf.Bytes())
	return buf.Bytes(), hex.EncodeToString(sum[:])
}

func installInto(t *testing.T, digest string) Install {
	t.Helper()
	return Install{
		Root: t.TempDir(),
		Pin: Pin{
			Revision:            "1400000",
			URL:                 "https://example.invalid/chrome-win.zip",
			Sha256:              digest,
			ExecutableInArchive: "chrome-win/chrome.exe",
		},
	}
}

func TestDownloadVerifiesAndExtracts(t *testing.T) {
	archive, digest := buildZip(t, map[string]string{
		"chrome-win/chrome.exe":    "MZ fake binary",
		"chrome-win/resources.pak": "resources",
	})
	install := installInto(t, digest)
	fetcher := &fakeFetcher{body: archive}

	if install.Present() {
		t.Fatal("a fresh root should not be present")
	}
	if err := install.Download(context.Background(), fetcher); err != nil {
		t.Fatalf("download: %v", err)
	}
	if !install.Present() {
		t.Fatalf("not present after download; expected %s", install.ExecutablePath())
	}
	if len(fetcher.URLs) != 1 || fetcher.URLs[0] != install.Pin.URL {
		t.Errorf("fetched %v, want the pinned url", fetcher.URLs)
	}
}

// TestAnUnverifiedInstallIsRefused. This archive becomes the process that renders hostile HTML while
// holding the owner's logins; TLS says who served it, not what it is.
func TestAnUnverifiedInstallIsRefused(t *testing.T) {
	archive, _ := buildZip(t, map[string]string{"chrome-win/chrome.exe": "MZ"})
	install := installInto(t, "") // no pinned digest
	err := install.Download(context.Background(), &fakeFetcher{body: archive})
	if !errors.Is(err, ErrNoDigest) {
		t.Fatalf("got %v, want ErrNoDigest", err)
	}
	if install.Present() {
		t.Fatal("something was installed anyway")
	}
}

func TestATamperedArchiveIsRefusedAndNothingIsWritten(t *testing.T) {
	archive, digest := buildZip(t, map[string]string{"chrome-win/chrome.exe": "MZ real"})
	tampered, _ := buildZip(t, map[string]string{"chrome-win/chrome.exe": "MZ evil"})
	_ = archive

	install := installInto(t, digest)
	err := install.Download(context.Background(), &fakeFetcher{body: tampered})
	if !errors.Is(err, ErrDigestMismatch) {
		t.Fatalf("got %v, want ErrDigestMismatch", err)
	}
	// The whole archive is hashed before anything is extracted, so a failed check must leave no
	// half-written browser behind for something to run later.
	if _, err := os.Stat(install.Dir()); !os.IsNotExist(err) {
		t.Fatalf("the install directory exists after a rejected archive: %v", err)
	}
}

// TestAZipCannotEscapeTheInstallDirectory. This is the one thing unpacked from off the machine.
func TestAZipCannotEscapeTheInstallDirectory(t *testing.T) {
	for _, evil := range []string{
		"../escaped.txt",
		"chrome-win/../../escaped.txt",
	} {
		t.Run(evil, func(t *testing.T) {
			archive, digest := buildZip(t, map[string]string{evil: "pwned"})
			install := installInto(t, digest)
			err := install.Download(context.Background(), &fakeFetcher{body: archive})
			if err == nil {
				t.Fatal("the archive was extracted")
			}
			outside := filepath.Join(filepath.Dir(install.Dir()), "escaped.txt")
			if _, statErr := os.Stat(outside); statErr == nil {
				t.Fatalf("a file was written outside the install directory: %s", outside)
			}
		})
	}
}

func TestDownloadIsANoOpWhenAlreadyPresent(t *testing.T) {
	archive, digest := buildZip(t, map[string]string{"chrome-win/chrome.exe": "MZ"})
	install := installInto(t, digest)
	if err := install.Download(context.Background(), &fakeFetcher{body: archive}); err != nil {
		t.Fatalf("first download: %v", err)
	}
	// A fetcher that would fail if used at all.
	second := &fakeFetcher{err: errors.New("should not be fetched")}
	if err := install.Download(context.Background(), second); err != nil {
		t.Fatalf("second download: %v", err)
	}
	if len(second.URLs) != 0 {
		t.Errorf("re-fetched an installed Chromium: %v", second.URLs)
	}
}

// TestTheLayoutMatchesTheSpec pins spec §5.6, including the revision-scoped directory: a version
// bump must be a new directory, not an overwrite of the binary a running session is executing.
func TestTheLayoutMatchesTheSpec(t *testing.T) {
	install := installInto(t, "abc")
	if filepath.Base(install.Dir()) != "chromium-1400000" {
		t.Errorf("install dir: %s", install.Dir())
	}
	// The profiles live beside the binary, never inside it: `profile.Store` is given this directory
	// and is allowed to delete trees under it, so a layout where the Chromium sat below would put the
	// browser within reach of the sweeper. What the profile directories are CALLED is tested in
	// package profile, which is the only place that names them.
	if filepath.Dir(install.ProfilesDir()) != install.Root {
		t.Errorf("profiles directory %s is not directly under the root", install.ProfilesDir())
	}
	if relative, err := filepath.Rel(install.ProfilesDir(), install.Dir()); err == nil &&
		!strings.HasPrefix(relative, "..") {
		t.Errorf("the pinned Chromium (%s) sits inside the profiles directory", install.Dir())
	}
}

func TestPresentIsFalseForAnEmptyExecutable(t *testing.T) {
	install := installInto(t, "abc")
	if err := os.MkdirAll(filepath.Dir(install.ExecutablePath()), 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(install.ExecutablePath(), nil, 0o755); err != nil {
		t.Fatalf("write: %v", err)
	}
	if install.Present() {
		t.Fatal("a zero-byte chrome.exe counted as installed")
	}
}
