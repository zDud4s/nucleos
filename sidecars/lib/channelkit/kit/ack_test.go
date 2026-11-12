package kit

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"reflect"
	"testing"
	"time"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

// ackFixture decodes acks/<name> from the shared fixtures.
func ackFixture(t *testing.T, name string) contract.Ack {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), "acks/"+name)
	if err != nil {
		t.Fatal(err)
	}
	a, err := contract.DecodeAck(b)
	if err != nil {
		t.Fatal(err)
	}
	return a
}

func mustAck(t *testing.T, refs []string, err error) contract.Ack {
	t.Helper()
	a, send := AckFromResult(refs, err)
	if !send {
		t.Fatal("send = false, want true")
	}
	return a
}

func TestAckMatchesFixtures(t *testing.T) {
	cases := []struct {
		fixture string
		refs    []string
		err     error
	}{
		{"ok.json", []string{"993", "994"}, nil},
		{"ok-no-refs.json", nil, nil},
		{"ok-empty-refs.json", []string{}, nil},
		{"failed-retryable.json", nil, Retryable("rate_limited", 45*time.Second)},
		{"failed-retryable-no-delay.json", nil, Retryable("network", 0)},
		{"failed-permanent.json", nil, Permanent("thread_not_found")},
		{"unsupported-capability.json", nil, Permanent("unsupported_capability")},
	}
	for _, c := range cases {
		t.Run(c.fixture, func(t *testing.T) {
			got := mustAck(t, c.refs, c.err)
			want := ackFixture(t, c.fixture)
			if len(want.PlatformRefs) == 0 {
				want.PlatformRefs = nil
			}
			if len(got.PlatformRefs) == 0 {
				got.PlatformRefs = nil
			}
			if !reflect.DeepEqual(got, want) {
				t.Fatalf("got %+v (failed %+v), want %+v (failed %+v)", got, got.Failed, want, want.Failed)
			}
		})
	}
}

func failedOf(t *testing.T, err error) *contract.Failed {
	t.Helper()
	a := mustAck(t, nil, err)
	if a.OK || a.Failed == nil {
		t.Fatalf("ack = %+v, want failed", a)
	}
	// Every produced ack must be valid on the wire.
	b, merr := a.Marshal()
	if merr != nil {
		t.Fatal(merr)
	}
	if _, derr := contract.DecodeAck(b); derr != nil {
		t.Fatalf("ack %s does not decode: %v", b, derr)
	}
	return a.Failed
}

func TestAckRetryAfterClampedToMax(t *testing.T) {
	f := failedOf(t, Retryable("rate_limited", 48*time.Hour))
	if f.RetryAfterMS == nil || *f.RetryAfterMS != contract.MaxRetryAfterMS {
		t.Fatalf("retry_after_ms = %v, want %d", f.RetryAfterMS, contract.MaxRetryAfterMS)
	}
}

func TestAckRetryAfterNegativeOmitted(t *testing.T) {
	f := failedOf(t, Retryable("network", -5*time.Second))
	if !f.Retryable || f.RetryAfterMS != nil {
		t.Fatalf("failed = %+v, want retryable without retry_after_ms", f)
	}
}

func TestAckRetryAfterSubMillisecondRoundsUp(t *testing.T) {
	f := failedOf(t, Retryable("network", 100*time.Microsecond))
	if f.RetryAfterMS == nil || *f.RetryAfterMS != 1 {
		t.Fatalf("retry_after_ms = %v, want 1", f.RetryAfterMS)
	}
	f = failedOf(t, Retryable("network", 1500*time.Microsecond))
	if f.RetryAfterMS == nil || *f.RetryAfterMS != 2 {
		t.Fatalf("retry_after_ms = %v, want 2", f.RetryAfterMS)
	}
}

func TestAckReasonNormalized(t *testing.T) {
	cases := map[string]string{
		"Rate limited":  "rate_limited",
		"rate_limited":  "rate_limited",
		"  Bad--Thing ": "bad_thing",
		"":              "error",
		"!!!":           "error",
		"123abc":        "abc",
	}
	for in, want := range cases {
		if got := failedOf(t, Permanent(in)).Reason; got != want {
			t.Errorf("Permanent(%q) reason = %q, want %q", in, got, want)
		}
	}
	long := failedOf(t, Permanent(string(make([]byte, 0))+fmt.Sprintf("%0100d", 7)))
	if long.Reason != "error" && len(long.Reason) > 64 {
		t.Fatalf("reason too long: %d", len(long.Reason))
	}
	long = failedOf(t, Permanent("a"+fmt.Sprintf("%0100d", 7)))
	if len(long.Reason) != 64 {
		t.Fatalf("reason length = %d, want 64", len(long.Reason))
	}
}

func TestAckUnclassifiedErrorIsRetryableError(t *testing.T) {
	f := failedOf(t, errors.New("boom"))
	want := contract.Failed{Retryable: true, Reason: "error"}
	if !reflect.DeepEqual(*f, want) {
		t.Fatalf("failed = %+v, want %+v", *f, want)
	}
}

func TestAckEmptyRefsNeverSerializeEmptyString(t *testing.T) {
	for _, refs := range [][]string{nil, {}, {""}, {"", ""}} {
		a := mustAck(t, refs, nil)
		b, err := a.Marshal()
		if err != nil {
			t.Fatal(err)
		}
		if string(b) != `{"ok":true}` {
			t.Fatalf("refs %q -> %s", refs, b)
		}
	}
	a := mustAck(t, []string{"1", "", "2"}, nil)
	if !reflect.DeepEqual(a.PlatformRefs, []string{"1", "2"}) {
		t.Fatalf("refs = %q", a.PlatformRefs)
	}
}

func TestAckWrappedErrorKeepsClassification(t *testing.T) {
	err := fmt.Errorf("send: %w", Retryable("rate_limited", 45*time.Second))
	got := mustAck(t, nil, err)
	want := ackFixture(t, "failed-retryable.json")
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %+v, want %+v", got.Failed, want.Failed)
	}
	err = fmt.Errorf("outer: %w", fmt.Errorf("inner: %w", Permanent("thread_not_found")))
	if got := mustAck(t, nil, err); !reflect.DeepEqual(got, ackFixture(t, "failed-permanent.json")) {
		t.Fatalf("got %+v", got.Failed)
	}
	var ee *ExecError
	if !errors.As(Permanent("x"), &ee) || ee.Retryable {
		t.Fatalf("Permanent is not a non-retryable *ExecError: %+v", ee)
	}
}

func TestAckContextCanceledSendsNothing(t *testing.T) {
	for _, err := range []error{
		context.Canceled,
		fmt.Errorf("send: %w", context.Canceled),
		fmt.Errorf("x: %w", errors.Join(errors.New("a"), context.Canceled)),
	} {
		if _, send := AckFromResult(nil, err); send {
			t.Fatalf("send = true for %v", err)
		}
	}
	// A deadline is an ordinary retryable failure, not a shutdown.
	if _, send := AckFromResult(nil, context.DeadlineExceeded); !send {
		t.Fatal("DeadlineExceeded must still be acked")
	}
}
