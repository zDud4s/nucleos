package kit

import (
	"context"
	"errors"
	"strings"
	"time"

	"nucleoschannelkit/contract"
)

// ExecError is an execution failure classified for the ack: retryable or permanent,
// with a machine-readable reason and, for retryable ones, an optional delay.
type ExecError struct {
	Retryable  bool
	Reason     string
	RetryAfter time.Duration
}

// Error implements error.
func (e *ExecError) Error() string {
	class := "permanent"
	if e.Retryable {
		class = "retryable"
	}
	return class + " execution error: " + e.Reason
}

// Retryable classifies a failure the core should retry. A retryAfter of zero (or less)
// means "no hint": the core applies only its own backoff.
func Retryable(reason string, retryAfter time.Duration) error {
	return &ExecError{Retryable: true, Reason: reason, RetryAfter: retryAfter}
}

// Permanent classifies a failure the core must not retry.
func Permanent(reason string) error {
	return &ExecError{Retryable: false, Reason: reason}
}

// AckFromResult maps the outcome of executing one action to its contract ack. send is
// false when no ack must be posted: the context was cancelled (shutdown), so the lease
// expires and the core redelivers. An error that is not an *ExecError is an unclassified
// failure: retryable with reason "error" (the core caps the attempts).
func AckFromResult(platformRefs []string, err error) (ack contract.Ack, send bool) {
	if err == nil {
		var refs []string
		for _, r := range platformRefs {
			if r != "" {
				refs = append(refs, r)
			}
		}
		return contract.Ack{OK: true, PlatformRefs: refs}, true
	}
	if errors.Is(err, context.Canceled) {
		return contract.Ack{}, false
	}
	f := &contract.Failed{Retryable: true, Reason: "error"}
	var ee *ExecError
	if errors.As(err, &ee) {
		f.Retryable = ee.Retryable
		f.Reason = normalizeReason(ee.Reason)
		if ee.Retryable && ee.RetryAfter > 0 {
			ms := uint64((ee.RetryAfter + time.Millisecond - 1) / time.Millisecond)
			if ee.RetryAfter > time.Duration(contract.MaxRetryAfterMS)*time.Millisecond {
				ms = contract.MaxRetryAfterMS
			}
			f.RetryAfterMS = &ms
		}
	}
	return contract.Ack{Failed: f}, true
}

// normalizeReason forces a reason into the grammar ^[a-z][a-z0-9_]{0,63}$: lowercase,
// every run of other characters becomes one underscore, leading non-letters and trailing
// underscores are dropped, and the result is cut to 64 bytes. Nothing left gives "error".
func normalizeReason(s string) string {
	var b strings.Builder
	pendingSep := false
	for _, r := range strings.ToLower(s) {
		switch {
		case r >= 'a' && r <= 'z':
			if pendingSep && b.Len() > 0 {
				b.WriteByte('_')
			}
			pendingSep = false
			b.WriteRune(r)
		case (r >= '0' && r <= '9') || r == '_':
			if b.Len() == 0 {
				continue
			}
			if r == '_' {
				pendingSep = true
				continue
			}
			if pendingSep {
				b.WriteByte('_')
			}
			pendingSep = false
			b.WriteRune(r)
		default:
			pendingSep = true
		}
	}
	out := b.String()
	if len(out) > 64 {
		out = strings.TrimRight(out[:64], "_")
	}
	if out == "" {
		return "error"
	}
	return out
}
