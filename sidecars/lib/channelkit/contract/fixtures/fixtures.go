// Package fixtures embeds the shared channel-contract fixtures, so the kit and every
// adapter test against the very files the core's serde tests read. Only tests import
// it, so no adapter binary carries these bytes.
package fixtures

import (
	"bytes"
	"embed"
	"encoding/json"
	"io/fs"
)

//go:embed index.json hello errors media events outbox acks renew capabilities
var files embed.FS

// FS is the fixture tree, rooted at the fixtures directory.
func FS() fs.FS { return files }

// Document is the decoded index.json (the type cannot be called Index: that is the loader).
type Document struct {
	ContractVersion string   `json:"contract_version"`
	Tags            []string `json:"tags"`
	Fixtures        []Entry  `json:"fixtures"`
}

// Entry describes one fixture file.
type Entry struct {
	File       string                     `json:"file"`
	Shape      string                     `json:"shape"`
	Covers     []string                   `json:"covers"`
	Valid      bool                       `json:"valid"`
	Error      *ExpectedError             `json:"error"`
	Canonical  string                     `json:"canonical"`
	Expect     map[string]json.RawMessage `json:"expect"`
	Compatible *bool                      `json:"compatible"`
	Go         *GoOverlay                 `json:"go"`
}

// ExpectedError is the error an invalid fixture must produce. A nil At means the key is
// absent; a pointer to "" means it is present and empty.
type ExpectedError struct {
	Code string  `json:"code"`
	At   *string `json:"at"`
}

// GoOverlay carries the places where the Go side differs from the shared expectation.
type GoOverlay struct {
	Valid       *bool     `json:"valid"`
	Canonical   string    `json:"canonical"`
	FallbackAck OptString `json:"fallback_ack"`
}

// OptString tells an absent key from an explicit null from a string.
type OptString struct {
	Set   bool
	Value *string
}

// UnmarshalJSON runs only when the key is present: null gives Set with a nil Value.
func (o *OptString) UnmarshalJSON(b []byte) error {
	o.Set = true
	if string(bytes.TrimSpace(b)) == "null" {
		o.Value = nil
		return nil
	}
	var s string
	if err := json.Unmarshal(b, &s); err != nil {
		return err
	}
	o.Value = &s
	return nil
}

// Index reads and decodes index.json. Numbers stay exact (UseNumber) wherever a value is
// decoded into an interface.
func Index() (Document, error) {
	var d Document
	b, err := fs.ReadFile(files, "index.json")
	if err != nil {
		return d, err
	}
	dec := json.NewDecoder(bytes.NewReader(b))
	dec.UseNumber()
	if err := dec.Decode(&d); err != nil {
		return Document{}, err
	}
	return d, nil
}
