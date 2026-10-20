package contract

// ErrorCode is one of the normative validation codes (annex C.2.1).
type ErrorCode string

const (
	CodeDecode     ErrorCode = "decode"
	CodeGrammar    ErrorCode = "grammar"
	CodeRange      ErrorCode = "range"
	CodeEmpty      ErrorCode = "empty"
	CodeRequiredIf ErrorCode = "required_if"
	CodeForbidden  ErrorCode = "forbidden"
	CodeOneOf      ErrorCode = "one_of"
	CodeUnique     ErrorCode = "unique"
	CodeValue      ErrorCode = "value"
)

// Error is a contract violation: a code and a JSON pointer ("" = the document).
type Error struct {
	Code ErrorCode
	At   string
}

// Error formats the violation as "<code> at <pointer>".
func (e *Error) Error() string {
	return string(e.Code) + " at " + e.At
}
