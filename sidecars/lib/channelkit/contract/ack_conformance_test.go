package contract_test

import (
	"encoding/json"
	"errors"
	"testing"

	"nucleoschannelkit/contract"
)

func init() {
	register("ack", func(b []byte) (any, error) {
		v, err := contract.DecodeAck(b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
	register("ack_response", func(b []byte) (any, error) {
		v, err := contract.DecodeAckResponse(b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
	register("renew_response", func(b []byte) (any, error) {
		v, err := contract.DecodeRenewResponse(b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
}

func wantAckErr(t *testing.T, file string, code contract.ErrorCode, at string) {
	t.Helper()
	_, err := contract.DecodeAck(readFixture(t, file))
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != code || ce.At != at {
		t.Fatalf("%s: err = %v, want %s at %q", file, err, code, at)
	}
}

func TestAckEmptyRefsEncodesToNoRefs(t *testing.T) {
	a, err := contract.DecodeAck(readFixture(t, "acks/ok-empty-refs.json"))
	if err != nil {
		t.Fatal(err)
	}
	if !a.OK || len(a.PlatformRefs) != 0 {
		t.Fatalf("unexpected ack %+v", a)
	}
	b, err := json.Marshal(a)
	if err != nil {
		t.Fatal(err)
	}
	if string(b) != `{"ok":true}` {
		t.Errorf("got %s", b)
	}
}

func TestAckMarshalMethodOmitsEmptyRefs(t *testing.T) {
	b, err := contract.Ack{OK: true, PlatformRefs: []string{}}.Marshal()
	if err != nil || string(b) != `{"ok":true}` {
		t.Fatalf("got %s, %v", b, err)
	}
	d := uint64(5)
	b, err = contract.Ack{Failed: &contract.Failed{Retryable: true, RetryAfterMS: &d, Reason: "network"}}.Marshal()
	if err != nil || string(b) != `{"failed":{"retryable":true,"retry_after_ms":5,"reason":"network"}}` {
		t.Fatalf("got %s, %v", b, err)
	}
}

func TestAckPermanentWithDelay(t *testing.T) {
	wantAckErr(t, "acks/permanent-with-delay.json", contract.CodeForbidden, "/failed/retry_after_ms")
}

func TestAckDelayCeiling(t *testing.T) {
	if contract.MaxRetryAfterMS != 86_400_000 {
		t.Fatalf("MaxRetryAfterMS = %d", contract.MaxRetryAfterMS)
	}
	wantAckErr(t, "acks/delay-too-long.json", contract.CodeRange, "/failed/retry_after_ms")
	ok := `{"failed":{"retryable":true,"retry_after_ms":86400000,"reason":"rate_limited"}}`
	a, err := contract.DecodeAck([]byte(ok))
	if err != nil || *a.Failed.RetryAfterMS != 86_400_000 {
		t.Fatalf("ceiling must be valid: %v", err)
	}
}

func TestAckResponseIgnoredPending(t *testing.T) {
	_, err := contract.DecodeAckResponse(readFixture(t, "acks/response-ignored-pending.json"))
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeForbidden || ce.At != "/ignored" {
		t.Fatalf("err = %v", err)
	}
}

func TestAckOKFalseWithoutFailed(t *testing.T) {
	wantAckErr(t, "acks/ok-false.json", contract.CodeValue, "/ok")
}

func TestAckMissingRetryable(t *testing.T) {
	_, err := contract.DecodeAck(readFixture(t, "acks/missing-retryable.json"))
	var ce *contract.Error
	if !errors.As(err, &ce) || ce.Code != contract.CodeDecode {
		t.Fatalf("err = %v", err)
	}
}

func TestAckOneOf(t *testing.T) {
	wantAckErr(t, "acks/empty.json", contract.CodeOneOf, "")
	wantAckErr(t, "acks/ok-and-failed.json", contract.CodeOneOf, "")
}
