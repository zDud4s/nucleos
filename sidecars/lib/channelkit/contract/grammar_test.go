package contract

import (
	"strings"
	"testing"
)

func TestGrammarRegexes(t *testing.T) {
	cases := []struct {
		name string
		ok   func(string) bool
		good []string
		bad  []string
	}{
		{"kind", kindRe.MatchString,
			[]string{"a", "telegram", "a-b1", "a" + strings.Repeat("b", 31)},
			[]string{"", "Telegram", "1a", "-a", "a_b", "a" + strings.Repeat("b", 32)}},
		{"media_id", mediaIDRe.MatchString,
			[]string{"m_1", "m_abc09", "m_" + strings.Repeat("a", 30)},
			[]string{"", "m_", "17", "m_A", "m_" + strings.Repeat("a", 31)}},
		{"token", tokenRe.MatchString,
			[]string{"tk_1", "tk_" + strings.Repeat("a", 61)},
			[]string{"", "tk_", "approve:12", "tk_" + strings.Repeat("a", 62), "tk_A"}},
		{"reason", reasonRe.MatchString,
			[]string{"a", "rate_limited", "a" + strings.Repeat("b", 63)},
			[]string{"", "Rate limited", "_a", "1a", "a" + strings.Repeat("b", 64)}},
		{"label", labelRe.MatchString,
			[]string{"sticker", "a_1", "a" + strings.Repeat("b", 31)},
			[]string{"", "Sticker!", "a-b", "a" + strings.Repeat("b", 32)}},
		{"mime", mimeRe.MatchString,
			[]string{"audio/ogg", "image/svg+xml", "a.b/c-d"},
			[]string{"", "audio", "audio/ogg; codecs=opus", "/ogg", "audio/", "Audio/ogg", "-a/b"}},
		{"pointer", pointerRe.MatchString,
			[]string{"", "/", "/a", "/a/0/b", "//"},
			[]string{"a", "conversation.ref", "a/b"}},
	}
	for _, c := range cases {
		for _, s := range c.good {
			if !c.ok(s) {
				t.Errorf("%s: %q should match", c.name, s)
			}
		}
		for _, s := range c.bad {
			if c.ok(s) {
				t.Errorf("%s: %q should not match", c.name, s)
			}
		}
	}
}

func TestGrammarCheckRef(t *testing.T) {
	cases := []struct {
		name string
		in   string
		want ErrorCode
	}{
		{"one byte", "x", ""},
		{"256 bytes", strings.Repeat("a", 256), ""},
		{"empty", "", CodeGrammar},
		{"257 bytes", strings.Repeat("a", 257), CodeGrammar},
		{"invalid utf8", "a\xffb", CodeGrammar},
		{"multibyte ok", "é", ""},
	}
	for _, c := range cases {
		e := checkRef(c.in, "/ref")
		if c.want == "" {
			if e != nil {
				t.Errorf("%s: got %v", c.name, e)
			}
			continue
		}
		if e == nil || e.Code != c.want || e.At != "/ref" {
			t.Errorf("%s: got %v", c.name, e)
		}
	}
}

func TestGrammarCheckAdapterToken(t *testing.T) {
	if e := checkAdapterToken("approve:12", "/t"); e != nil {
		t.Errorf("1-64 bytes accepted: %v", e)
	}
	if e := checkAdapterToken(strings.Repeat("a", 64), "/t"); e != nil {
		t.Errorf("64 bytes: %v", e)
	}
	for _, s := range []string{"", strings.Repeat("a", 65), "a\xff"} {
		if e := checkAdapterToken(s, "/t"); e == nil || e.Code != CodeGrammar {
			t.Errorf("%q: got %v", s, e)
		}
	}
}

func TestGrammarCheckAt(t *testing.T) {
	empty, ptr, bad := "", "/a/b", "conversation.ref"
	if e := checkAt(nil); e != nil {
		t.Errorf("absent: %v", e)
	}
	if e := checkAt(&empty); e != nil {
		t.Errorf("empty is the document: %v", e)
	}
	if e := checkAt(&ptr); e != nil {
		t.Errorf("pointer: %v", e)
	}
	if e := checkAt(&bad); e == nil || e.Code != CodeGrammar || e.At != "/at" {
		t.Errorf("bad pointer: %v", e)
	}
}

func TestGrammarConstructors(t *testing.T) {
	cases := []struct {
		name string
		fn   func(string) *Error
		code ErrorCode
	}{
		{"grammar", errGrammar, CodeGrammar},
		{"range", errRange, CodeRange},
		{"empty", errEmpty, CodeEmpty},
		{"required_if", errRequiredIf, CodeRequiredIf},
		{"forbidden", errForbidden, CodeForbidden},
		{"one_of", errOneOf, CodeOneOf},
		{"unique", errUnique, CodeUnique},
		{"value", errValue, CodeValue},
	}
	for _, c := range cases {
		e := c.fn("/x/0")
		if e == nil || e.Code != c.code || e.At != "/x/0" {
			t.Errorf("%s: got %v", c.name, e)
		}
		if d := c.fn(""); d == nil || d.Code != c.code || d.At != "" {
			t.Errorf("%s document: got %v", c.name, d)
		}
	}
}
