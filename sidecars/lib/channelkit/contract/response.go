package contract

import "unicode/utf8"

// Error codes of the 4xx bodies the core adds to the validation codes (T9).
const (
	ErrContractVersion = "contract_version"
	ErrChannelMismatch = "channel_mismatch"
	ErrMediaTooLarge   = "media_too_large"
	ErrNotLeased       = "not_leased"
)

// ErrorResponse is the body of every 4xx of the /channels/{channel}/* routes.
// At is a pointer so that "" (the document) differs from absent.
type ErrorResponse struct {
	Error           *string `json:"error" kit:"required"`
	At              *string `json:"at,omitempty"`
	Detail          string  `json:"detail,omitempty"`
	ContractVersion string  `json:"contract_version,omitempty"`
}

// Validate checks the error code grammar and the shape of the at pointer.
func (r *ErrorResponse) Validate() *Error {
	if !reasonRe.MatchString(*r.Error) {
		return errGrammar("/error")
	}
	if err := checkAt(r.At); err != nil {
		return err
	}
	if !utf8.ValidString(r.Detail) {
		return errGrammar("/detail")
	}
	return nil
}

// MediaResponse is the answer to a media upload.
type MediaResponse struct {
	MediaID *string `json:"media_id" kit:"required"`
}

// Validate checks the media id grammar.
func (r *MediaResponse) Validate() *Error {
	if !mediaIDRe.MatchString(*r.MediaID) {
		return errGrammar("/media_id")
	}
	return nil
}
