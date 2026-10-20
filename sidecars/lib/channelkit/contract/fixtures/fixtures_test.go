package fixtures

import (
	"bytes"
	"encoding/json"
	"io/fs"
	"sort"
	"strings"
	"testing"
	"unicode/utf8"
)

// treeFiles lists every *.json in the embedded tree except index.json, as slash paths.
func treeFiles(t *testing.T) []string {
	t.Helper()
	var out []string
	err := fs.WalkDir(FS(), ".", func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !d.IsDir() && strings.HasSuffix(p, ".json") && p != "index.json" {
			out = append(out, p)
		}
		return nil
	})
	if err != nil {
		t.Fatalf("walking the fixture tree: %v", err)
	}
	sort.Strings(out)
	return out
}

func TestIndexLoads(t *testing.T) {
	idx, err := Index()
	if err != nil {
		t.Fatalf("Index(): %v", err)
	}
	if idx.ContractVersion != "1.0.0" {
		t.Errorf("contract_version = %q, want 1.0.0", idx.ContractVersion)
	}
	if got := len(idx.Fixtures); got != 115 {
		t.Errorf("fixtures = %d, want 115", got)
	}
	if got := len(idx.Tags); got != 28 {
		t.Errorf("tags = %d, want 28", got)
	}
}

// TestNoOrphansNoGhosts owns rule C.11.2 no. 2: the *.json files of the tree, minus index.json,
// are exactly the files the index names.
func TestNoOrphansNoGhosts(t *testing.T) {
	idx, err := Index()
	if err != nil {
		t.Fatalf("Index(): %v", err)
	}
	listed := map[string]bool{}
	for _, e := range idx.Fixtures {
		if listed[e.File] {
			t.Errorf("index lists %s twice", e.File)
		}
		listed[e.File] = true
	}
	onDisk := map[string]bool{}
	for _, f := range treeFiles(t) {
		onDisk[f] = true
		if !listed[f] {
			t.Errorf("orphan: %s is on disk but not in the index", f)
		}
	}
	for f := range listed {
		if !onDisk[f] {
			t.Errorf("ghost: %s is in the index but not on disk", f)
		}
	}
}

func TestFilesUTF8NoBOM(t *testing.T) {
	bom := []byte{0xEF, 0xBB, 0xBF}
	files := append(treeFiles(t), "index.json")
	for _, f := range files {
		b, err := fs.ReadFile(FS(), f)
		if err != nil {
			t.Errorf("%s: %v", f, err)
			continue
		}
		if bytes.HasPrefix(b, bom) {
			t.Errorf("%s starts with a BOM", f)
		}
		if !utf8.Valid(b) {
			t.Errorf("%s is not valid UTF-8", f)
		}
	}
}

func TestIndexFilesParse(t *testing.T) {
	idx, err := Index()
	if err != nil {
		t.Fatalf("Index(): %v", err)
	}
	for _, e := range idx.Fixtures {
		b, err := fs.ReadFile(FS(), e.File)
		if err != nil {
			t.Errorf("%s: %v", e.File, err)
			continue
		}
		var v any
		perr := json.Unmarshal(b, &v)
		if e.File == "events/malformed.json" {
			if perr == nil {
				t.Errorf("%s must fail to parse", e.File)
			}
			continue
		}
		if perr != nil {
			t.Errorf("%s does not parse: %v", e.File, perr)
		}
	}
}

func TestOptString(t *testing.T) {
	var idx struct {
		A, B, C GoOverlay
	}
	src := `{"A":{},"B":{"fallback_ack":null},"C":{"fallback_ack":"x"}}`
	if err := json.Unmarshal([]byte(src), &idx); err != nil {
		t.Fatal(err)
	}
	if idx.A.FallbackAck.Set {
		t.Error("absent must be Set=false")
	}
	if !idx.B.FallbackAck.Set || idx.B.FallbackAck.Value != nil {
		t.Error("null must be Set=true, Value=nil")
	}
	if !idx.C.FallbackAck.Set || idx.C.FallbackAck.Value == nil || *idx.C.FallbackAck.Value != "x" {
		t.Error("string must be Set=true, Value=&x")
	}
}
