//go:build unix

package launch

import (
	"archive/zip"
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type modedEntry struct {
	name string
	body string
	mode os.FileMode
}

// buildModedZip writes entries in order with Unix modes, the way the Linux and macOS archives are made.
func buildModedZip(t *testing.T, entries []modedEntry) []byte {
	t.Helper()
	var buf bytes.Buffer
	writer := zip.NewWriter(&buf)
	for _, entry := range entries {
		header := &zip.FileHeader{Name: entry.name, Method: zip.Deflate}
		header.SetMode(entry.mode)
		file, err := writer.CreateHeader(header)
		if err != nil {
			t.Fatalf("create %s: %v", entry.name, err)
		}
		if _, err := file.Write([]byte(entry.body)); err != nil {
			t.Fatalf("write %s: %v", entry.name, err)
		}
	}
	if err := writer.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	return buf.Bytes()
}

func testInstall(t *testing.T) Install {
	t.Helper()
	return Install{Root: t.TempDir(), Pin: Pin{Revision: "test"}}
}

func TestExtractPreservesASymlinkInsideTheInstall(t *testing.T) {
	framework := "chrome-mac/Chromium.app/Contents/Frameworks/F.framework/"
	install := testInstall(t)
	err := install.extract(buildModedZip(t, []modedEntry{
		{framework + "Versions/A/F", "binary", 0o755},
		{framework + "Versions/Current", "A", os.ModeSymlink | 0o777},
		{framework + "F", "Versions/Current/F", os.ModeSymlink | 0o777},
	}))
	if err != nil {
		t.Fatalf("extract: %v", err)
	}
	link := filepath.Join(install.Dir(), filepath.FromSlash(framework+"Versions/Current"))
	if got, err := os.Readlink(link); err != nil || got != "A" {
		t.Fatalf("Versions/Current is not a symlink to A: %q, %v", got, err)
	}
	body, err := os.ReadFile(filepath.Join(install.Dir(), filepath.FromSlash(framework+"F")))
	if err != nil || string(body) != "binary" {
		t.Fatalf("the framework binary is not reachable through its links: %q, %v", body, err)
	}
}

func TestExtractRefusesASymlinkThatLeavesTheInstall(t *testing.T) {
	for _, evil := range []string{"../../outside", "/etc", "sub/../../outside"} {
		t.Run(evil, func(t *testing.T) {
			install := testInstall(t)
			err := install.extract(buildModedZip(t, []modedEntry{{"chrome-linux/evil", evil, os.ModeSymlink | 0o777}}))
			if err == nil {
				t.Fatal("a symlink pointing out of the install was extracted")
			}
			if _, statErr := os.Lstat(filepath.Join(install.Dir(), "chrome-linux", "evil")); !os.IsNotExist(statErr) {
				t.Fatalf("something was written at the refused link's path: %v", statErr)
			}
		})
	}
}

func TestExtractRefusesAnEntryThatReachesThroughASymlink(t *testing.T) {
	install := testInstall(t)
	err := install.extract(buildModedZip(t, []modedEntry{
		{"chrome-linux/here", ".", os.ModeSymlink | 0o777},
		{"chrome-linux/here/payload", "written through a link", 0o644},
	}))
	if err == nil || !strings.Contains(err.Error(), "through the symlink") {
		t.Fatalf("got %v, want a refusal to write through the symlink", err)
	}
	if _, statErr := os.Lstat(filepath.Join(install.Dir(), "chrome-linux", "payload")); !os.IsNotExist(statErr) {
		t.Fatalf("the payload landed where the link points: %v", statErr)
	}
}

func TestExtractKeepsTheArchivesPermissionBits(t *testing.T) {
	install := testInstall(t)
	err := install.extract(buildModedZip(t, []modedEntry{
		{"chrome-linux/resources.pak", "data", 0o644},
		{"chrome-linux/chrome", "binary", 0o755},
		{"chrome-linux/loose", "data", 0o777},
	}))
	if err != nil {
		t.Fatalf("extract: %v", err)
	}
	for name, want := range map[string]os.FileMode{"resources.pak": 0o644, "chrome": 0o755, "loose": 0o755} {
		info, err := os.Stat(filepath.Join(install.Dir(), "chrome-linux", name))
		if err != nil || info.Mode().Perm() != want {
			t.Errorf("%s: %v, %v; want %o", name, info, err, want)
		}
	}
}
