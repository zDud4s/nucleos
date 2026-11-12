package contract

import (
	"encoding/json"
	"errors"
	"testing"
)

type inner struct {
	Refs []string `json:"platform_refs,omitempty"`
}

type rawDoc struct {
	Type  *string      `json:"type" kit:"required"`
	Note  *string      `json:"note,omitempty"`
	Count *json.Number `json:"count,omitempty"`
	Sub   *inner       `json:"sub,omitempty"`
}

func (rawDoc) Validate() *Error { return nil }

func code(t *testing.T, err error) ErrorCode {
	t.Helper()
	var e *Error
	if !errors.As(err, &e) {
		t.Fatalf("want *Error, got %v", err)
	}
	return e.Code
}

func TestDecodeStrictErrors(t *testing.T) {
	cases := map[string]string{
		"duplicate":  `{"type":"a","type":"b"}`,
		"case":       `{"Type":"a","type":"a"}`,
		"unknown":    `{"type":"a","extra":1}`,
		"nested":     `{"type":"a","sub":{"zzz":1}}`,
		"missing":    `{"note":"x"}`,
		"truncated":  `{"type":"a"`,
		"wrong type": `{"type":1}`,
		"trailing":   `{"type":"a"} {}`,
		"dup nested": `{"type":"a","sub":{"platform_refs":[],"platform_refs":[]}}`,
	}
	for name, in := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := DecodeStrict[rawDoc]([]byte(in))
			if err == nil || code(t, err) != CodeDecode {
				t.Fatalf("want decode, got %v", err)
			}
		})
	}
}

func TestDecodeStrictOK(t *testing.T) {
	d, err := DecodeStrict[rawDoc]([]byte(`{"type":"a","count":12345678901234567890,"sub":{"platform_refs":[]}}`))
	if err != nil {
		t.Fatal(err)
	}
	if *d.Type != "a" || d.Count.String() != "12345678901234567890" {
		t.Fatalf("bad decode %+v", d)
	}
}

func TestDecodeLenient(t *testing.T) {
	d, err := DecodeLenient[rawDoc]([]byte(`{"type":"a","Note":"x","extra":1,"sub":{"zzz":1}}`))
	if err != nil {
		t.Fatal(err)
	}
	if d.Note != nil {
		t.Fatal("case-mismatched key must be ignored")
	}
	for _, in := range []string{`{"note":"x"}`, `{"type":"a","type":"b"}`, `{"type":"a"`, `{"type":3}`} {
		if _, err := DecodeLenient[rawDoc]([]byte(in)); err == nil || code(t, err) != CodeDecode {
			t.Fatalf("%s: want decode, got %v", in, err)
		}
	}
}

type failing struct {
	A *string `json:"a" kit:"required"`
}

func (failing) Validate() *Error { return &Error{Code: CodeRange, At: "/a"} }

func TestDecodeRunsValidate(t *testing.T) {
	_, err := DecodeStrict[failing]([]byte(`{"a":"x"}`))
	if err == nil || code(t, err) != CodeRange {
		t.Fatalf("got %v", err)
	}
}

func TestDecodeErrorFormat(t *testing.T) {
	e := &Error{Code: CodeGrammar, At: "/x/0"}
	if e.Error() != "grammar at /x/0" {
		t.Fatal(e.Error())
	}
	want := []ErrorCode{"decode", "grammar", "range", "empty", "required_if", "forbidden", "one_of", "unique", "value"}
	got := []ErrorCode{CodeDecode, CodeGrammar, CodeRange, CodeEmpty, CodeRequiredIf, CodeForbidden, CodeOneOf, CodeUnique, CodeValue}
	for i := range want {
		if want[i] != got[i] {
			t.Fatal(want[i], got[i])
		}
	}
}
