package main

import (
	"bytes"
	"encoding/binary"
	"path/filepath"
	"testing"
	"unicode/utf16"
)

// Run with: cd sidecars/tools/winres && go test main.go main_test.go
func TestRoleForEveryDiscoveredSidecar(t *testing.T) {
	mods, err := filepath.Glob(filepath.Join("..", "..", "*", "go.mod"))
	if err != nil || len(mods) == 0 {
		t.Fatalf("no sidecars discovered: %v", err)
	}
	known := map[string]string{
		"email": "Email", "telegram": "Telegram", "web": "Web", "echo": "Echo",
		"browser": "Browser", "quota": "Quota", "remote": "Remote",
	}
	for _, m := range mods {
		name := filepath.Base(filepath.Dir(m))
		role := Role(name)
		if want, ok := known[name]; ok && role != want {
			t.Errorf("Role(%q) = %q, want %q", name, role, want)
		}
		if role == "" || role[0] < 'A' || role[0] > 'Z' {
			t.Errorf("Role(%q) = %q is not capitalised", name, role)
		}
		if got := Description(name); got != "NucleOS — "+role {
			t.Errorf("Description(%q) = %q", name, got)
		}
	}
}

func TestSysoCarriesUTF16Description(t *testing.T) {
	payload, err := VersionInfo("email", "0.1.0")
	if err != nil {
		t.Fatal(err)
	}
	want := utf16.Encode([]rune("NucleOS — Email"))
	var enc bytes.Buffer
	for _, c := range want {
		binary.Write(&enc, binary.LittleEndian, c)
	}
	if !bytes.Contains(payload, enc.Bytes()) {
		t.Fatal("UTF-16 FileDescription missing from payload")
	}
	if binary.LittleEndian.Uint16(payload) != uint16(len(payload)) {
		t.Fatal("root wLength does not cover the payload")
	}
	obj, err := Syso("amd64", payload)
	if err != nil || binary.LittleEndian.Uint16(obj) != 0x8664 {
		t.Fatalf("bad syso: %v", err)
	}
	if _, err := ParseVersion("1.2.3-beta"); err != nil {
		t.Fatal(err)
	}
}
