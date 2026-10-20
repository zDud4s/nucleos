package contract

import (
	"encoding/json"
	"fmt"
	"time"
)

// MaxRetryAfterMS is the ceiling of a failure's retry_after_ms (T8, Q5): 24 hours.
// It is the single constant shared with the core.
const MaxRetryAfterMS = 86_400_000

// failedRaw is the wire form of a failure; pointers tell absent from zero.
type failedRaw struct {
	Retryable    *bool   `json:"retryable" kit:"required"`
	RetryAfterMS *uint64 `json:"retry_after_ms,omitempty"`
	Reason       *string `json:"reason" kit:"required"`
}

// ackRaw is the wire form of an ack. It tells ok:false, platform_refs:[] and an
// absent key apart; DecodeAck converts it to the public Ack.
type ackRaw struct {
	OK           *bool      `json:"ok,omitempty"`
	PlatformRefs *[]string  `json:"platform_refs,omitempty"`
	Failed       *failedRaw `json:"failed,omitempty"`
}

// Validate checks the ok/failed alternative, the refs and the failure rules.
func (a *ackRaw) Validate() *Error {
	if (a.OK == nil) == (a.Failed == nil) {
		return errOneOf("")
	}
	if a.OK != nil {
		if !*a.OK {
			return errValue("/ok")
		}
		if a.PlatformRefs != nil {
			for i, ref := range *a.PlatformRefs {
				at := fmt.Sprintf("/platform_refs/%d", i)
				if ref == "" {
					return errEmpty(at)
				}
				if err := checkRef(ref, at); err != nil {
					return err
				}
			}
		}
		return nil
	}
	if a.PlatformRefs != nil {
		return errForbidden("/platform_refs")
	}
	f := a.Failed
	if !reasonRe.MatchString(*f.Reason) {
		return errGrammar("/failed/reason")
	}
	if f.RetryAfterMS != nil {
		if !*f.Retryable {
			return errForbidden("/failed/retry_after_ms")
		}
		if *f.RetryAfterMS > MaxRetryAfterMS {
			return errRange("/failed/retry_after_ms")
		}
	}
	return nil
}

// Failed is the failure branch of an ack.
type Failed struct {
	Retryable    bool    `json:"retryable"`
	RetryAfterMS *uint64 `json:"retry_after_ms,omitempty"`
	Reason       string  `json:"reason"`
}

// Ack is the adapter's answer to one outbox action: success (OK) or Failed.
// Empty PlatformRefs are omitted when encoding (T6).
type Ack struct {
	OK           bool
	PlatformRefs []string
	Failed       *Failed
}

type ackWire struct {
	OK           *bool    `json:"ok,omitempty"`
	PlatformRefs []string `json:"platform_refs,omitempty"`
	Failed       *Failed  `json:"failed,omitempty"`
}

// MarshalJSON encodes the ack in its wire form.
func (a Ack) MarshalJSON() ([]byte, error) {
	w := ackWire{PlatformRefs: a.PlatformRefs, Failed: a.Failed}
	if a.Failed == nil {
		ok := true
		w.OK = &ok
	}
	return json.Marshal(w)
}

// Marshal returns the wire form of the ack.
func (a Ack) Marshal() ([]byte, error) { return a.MarshalJSON() }

// DecodeAck decodes an ack strictly (an input shape).
func DecodeAck(b []byte) (Ack, error) {
	r, err := DecodeStrict[ackRaw](b)
	if err != nil {
		return Ack{}, err
	}
	var a Ack
	if r.Failed != nil {
		a.Failed = &Failed{Retryable: *r.Failed.Retryable, RetryAfterMS: r.Failed.RetryAfterMS, Reason: *r.Failed.Reason}
		return a, nil
	}
	a.OK = true
	if r.PlatformRefs != nil {
		a.PlatformRefs = *r.PlatformRefs
	}
	return a, nil
}

// AckState is the core's state for an acked action. Unknown states do not decode.
type AckState string

const (
	AckDone    AckState = "done"
	AckPending AckState = "pending"
	AckFailed  AckState = "failed"
)

// UnmarshalJSON accepts only the known states (Q9).
func (s *AckState) UnmarshalJSON(b []byte) error {
	var v string
	if err := json.Unmarshal(b, &v); err != nil {
		return err
	}
	switch AckState(v) {
	case AckDone, AckPending, AckFailed:
		*s = AckState(v)
		return nil
	}
	return fmt.Errorf("unknown ack state %q", v)
}

type ackResponseRaw struct {
	State   *AckState `json:"state" kit:"required"`
	Ignored bool      `json:"ignored,omitempty"`
}

// Validate checks that ignored is not set on a pending action.
func (r *ackResponseRaw) Validate() *Error {
	if r.Ignored && *r.State == AckPending {
		return errForbidden("/ignored")
	}
	return nil
}

// AckResponse is the core's answer to an ack (an output shape).
type AckResponse struct {
	State   AckState `json:"state"`
	Ignored bool     `json:"ignored,omitempty"`
}

// DecodeAckResponse decodes an ack response leniently (an output shape).
func DecodeAckResponse(b []byte) (AckResponse, error) {
	r, err := DecodeLenient[ackResponseRaw](b)
	if err != nil {
		return AckResponse{}, err
	}
	return AckResponse{State: *r.State, Ignored: r.Ignored}, nil
}

type renewResponseRaw struct {
	LeaseUntil *string `json:"lease_until" kit:"required"`
}

// Validate checks that lease_until is RFC 3339.
func (r *renewResponseRaw) Validate() *Error {
	if _, err := time.Parse(time.RFC3339, *r.LeaseUntil); err != nil {
		return errGrammar("/lease_until")
	}
	return nil
}

// RenewResponse is the core's answer to a lease renewal (an output shape).
type RenewResponse struct {
	LeaseUntil string `json:"lease_until"`
}

// DecodeRenewResponse decodes a renew response leniently (an output shape).
func DecodeRenewResponse(b []byte) (RenewResponse, error) {
	r, err := DecodeLenient[renewResponseRaw](b)
	if err != nil {
		return RenewResponse{}, err
	}
	return RenewResponse{LeaseUntil: *r.LeaseUntil}, nil
}
