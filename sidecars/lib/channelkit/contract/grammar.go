package contract

import (
	"regexp"
	"unicode/utf8"
)

// Grammars of annex C.2 (T7). Each regex is the single owner of its grammar;
// shape validators use them and never redefine them.
var (
	// kindRe is a channel or conversation kind.
	kindRe = regexp.MustCompile(`^[a-z][a-z0-9-]{0,31}$`)
	// mediaIDRe is a media id issued by the core.
	mediaIDRe = regexp.MustCompile(`^m_[0-9a-z]{1,30}$`)
	// tokenRe is a button token as the core issues it (at most 64 bytes).
	tokenRe = regexp.MustCompile(`^tk_[0-9a-z]{1,61}$`)
	// reasonRe is an ack failure reason and an error code.
	reasonRe = regexp.MustCompile(`^[a-z][a-z0-9_]{0,63}$`)
	// labelRe is the label of an "unsupported" part.
	labelRe = regexp.MustCompile(`^[a-z][a-z0-9_]{0,31}$`)
	// mimeRe is a MIME type without parameters.
	mimeRe = regexp.MustCompile(`^[a-z0-9][a-z0-9.+-]*/[a-z0-9][a-z0-9.+-]*$`)
	// pointerRe is a JSON pointer; "" designates the document.
	pointerRe = regexp.MustCompile(`^(/[^/]*)*$`)
)

const (
	maxRefBytes   = 256
	maxTokenBytes = 64
)

// checkBytes reports a grammar error unless s is valid UTF-8 of 1..max bytes.
func checkBytes(s string, max int, at string) *Error {
	if len(s) < 1 || len(s) > max || !utf8.ValidString(s) {
		return errGrammar(at)
	}
	return nil
}

// checkRef validates a ref-like string: 1-256 bytes of UTF-8.
func checkRef(s, at string) *Error { return checkBytes(s, maxRefBytes, at) }

// checkAdapterToken validates a button token on the adapter side: 1-64 bytes,
// without requiring the "tk_" prefix.
func checkAdapterToken(s, at string) *Error { return checkBytes(s, maxTokenBytes, at) }

// checkAt validates the "at" pointer of an error body. nil (absent) is valid,
// and so is "" (the document); anything else must match pointerRe.
func checkAt(at *string) *Error {
	if at == nil || (utf8.ValidString(*at) && pointerRe.MatchString(*at)) {
		return nil
	}
	return errGrammar("/at")
}

func errGrammar(at string) *Error    { return &Error{Code: CodeGrammar, At: at} }
func errRange(at string) *Error      { return &Error{Code: CodeRange, At: at} }
func errEmpty(at string) *Error      { return &Error{Code: CodeEmpty, At: at} }
func errRequiredIf(at string) *Error { return &Error{Code: CodeRequiredIf, At: at} }
func errForbidden(at string) *Error  { return &Error{Code: CodeForbidden, At: at} }
func errOneOf(at string) *Error      { return &Error{Code: CodeOneOf, At: at} }
func errUnique(at string) *Error     { return &Error{Code: CodeUnique, At: at} }
func errValue(at string) *Error      { return &Error{Code: CodeValue, At: at} }
