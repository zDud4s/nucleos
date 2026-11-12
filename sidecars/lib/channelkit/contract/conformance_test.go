package contract_test

import (
	"bytes"
	"encoding/json"
	"errors"
	"io/fs"
	"path"
	"reflect"
	"sort"
	"strings"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

// decoders maps a fixture shape to the decoder under test. A decoder returns a value
// that json.Marshal turns back into the contract's wire form, or a *contract.Error.
// Later chunks fill it through register from their own *_conformance_test.go files.
var decoders = map[string]func([]byte) (any, error){}

// register binds shape to its decoder; it is meant to be called from a test init().
func register(shape string, fn func([]byte) (any, error)) {
	if _, dup := decoders[shape]; dup {
		panic("conformance: decoder registered twice for shape " + shape)
	}
	decoders[shape] = fn
}

func loadIndex(t *testing.T) fixtures.Document {
	t.Helper()
	idx, err := fixtures.Index()
	if err != nil {
		t.Fatalf("Index(): %v", err)
	}
	return idx
}

func readFixture(t *testing.T, file string) []byte {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), file)
	if err != nil {
		t.Fatalf("reading %s: %v", file, err)
	}
	return b
}

// parseValue interprets JSON bytes with exact numbers, so values compare, never bytes.
func parseValue(b []byte) (any, error) {
	dec := json.NewDecoder(bytes.NewReader(b))
	dec.UseNumber()
	var v any
	if err := dec.Decode(&v); err != nil {
		return nil, err
	}
	return v, nil
}

func TestIndexVersion(t *testing.T) {
	if got := loadIndex(t).ContractVersion; got != contract.Version {
		t.Errorf("index contract_version = %q, contract.Version = %q", got, contract.Version)
	}
}

func TestCoverageIsComplete(t *testing.T) {
	idx := loadIndex(t)
	tags := map[string]bool{}
	for _, tag := range idx.Tags {
		tags[tag] = true
	}
	valid := map[string]bool{}
	invalid := map[string]bool{}
	byFile := map[string]fixtures.Entry{}
	for _, e := range idx.Fixtures {
		byFile[e.File] = e
		for _, c := range e.Covers {
			if !tags[c] {
				t.Errorf("%s covers %q, which is not in tags", e.File, c)
			}
			if e.Valid {
				valid[c] = true
			} else {
				invalid[c] = true
			}
		}
	}
	for _, tag := range idx.Tags {
		if !valid[tag] {
			t.Errorf("tag %q has no valid fixture", tag)
		}
		if !invalid[tag] {
			t.Errorf("tag %q has no invalid fixture", tag)
		}
	}
	for _, e := range idx.Fixtures {
		if e.Canonical == "" {
			continue
		}
		target, ok := byFile[e.Canonical]
		switch {
		case !ok:
			t.Errorf("%s: canonical %s is not in the index", e.File, e.Canonical)
		case !target.Valid:
			t.Errorf("%s: canonical %s is not a valid fixture", e.File, e.Canonical)
		case target.Shape != e.Shape:
			t.Errorf("%s: canonical %s has shape %s, want %s", e.File, e.Canonical, target.Shape, e.Shape)
		}
	}
}

// effective applies the Go overlay of the index on top of the shared expectation.
func effective(e fixtures.Entry) (valid bool, canonical string) {
	valid = e.Valid
	canonical = e.Canonical
	if e.Go != nil {
		if e.Go.Valid != nil {
			valid = *e.Go.Valid
		}
		if e.Go.Canonical != "" {
			canonical = e.Go.Canonical
		}
	}
	if canonical == "" {
		canonical = e.File
	}
	return valid, canonical
}

func TestConformance(t *testing.T) {
	idx := loadIndex(t)
	for _, e := range idx.Fixtures {
		if e.Shape == "scenario" {
			continue // no decoder; chunk B15 owns the scenarios
		}
		name := e.Shape + "/" + strings.TrimSuffix(path.Base(e.File), ".json")
		t.Run(name, func(t *testing.T) {
			fn, ok := decoders[e.Shape]
			if !ok {
				t.Fatalf("no decoder registered for shape %s", e.Shape)
			}
			valid, canonical := effective(e)
			raw := readFixture(t, e.File)
			got, err := fn(raw)
			if valid {
				checkValid(t, e, canonical, got, err)
			} else {
				checkInvalid(t, e, err)
			}
		})
	}
}

func checkValid(t *testing.T, e fixtures.Entry, canonical string, got any, err error) {
	t.Helper()
	if err != nil {
		t.Fatalf("decode failed, want ok: %v", err)
	}
	out, merr := json.Marshal(got)
	if merr != nil {
		t.Fatalf("re-encoding the decoded value: %v", merr)
	}
	gotV, perr := parseValue(out)
	if perr != nil {
		t.Fatalf("re-encoded value is not JSON: %v", perr)
	}
	wantV, perr := parseValue(readFixture(t, canonical))
	if perr != nil {
		t.Fatalf("canonical %s does not parse: %v", canonical, perr)
	}
	if !reflect.DeepEqual(gotV, wantV) {
		t.Errorf("re-encoded value differs from %s\n got: %s\nwant: %s", canonical, out, readFixture(t, canonical))
	}
	ptrs := make([]string, 0, len(e.Expect))
	for p := range e.Expect {
		ptrs = append(ptrs, p)
	}
	sort.Strings(ptrs)
	for _, p := range ptrs {
		want, perr := parseValue(e.Expect[p])
		if perr != nil {
			t.Errorf("expect %q: bad value in the index: %v", p, perr)
			continue
		}
		have, found := resolvePointer(gotV, p)
		if !found {
			t.Errorf("expect %q: pointer does not resolve in the re-encoded value", p)
			continue
		}
		if !reflect.DeepEqual(have, want) {
			t.Errorf("expect %q: got %v, want %v", p, have, want)
		}
	}
}

func checkInvalid(t *testing.T, e fixtures.Entry, err error) {
	t.Helper()
	if e.Error == nil {
		t.Fatalf("index entry is invalid but carries no error")
	}
	if err == nil {
		t.Fatalf("decode succeeded, want %s", e.Error.Code)
	}
	var ce *contract.Error
	if !errors.As(err, &ce) {
		t.Fatalf("error is %T (%v), want *contract.Error", err, err)
	}
	if string(ce.Code) != e.Error.Code {
		t.Errorf("code = %q, want %q (at %q)", ce.Code, e.Error.Code, ce.At)
	}
	if e.Error.At != nil && ce.At != *e.Error.At {
		t.Errorf("at = %q, want %q", ce.At, *e.Error.At)
	}
}
