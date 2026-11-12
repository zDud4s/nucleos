package contract_test

import (
	"encoding/json"
	"testing"

	"nucleoschannelkit/contract"
)

func init() {
	register("hello", func(b []byte) (any, error) {
		v, err := contract.DecodeStrict[contract.Hello](b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
	register("hello_response", func(b []byte) (any, error) {
		v, err := contract.DecodeLenient[contract.HelloResponse](b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
	register("error_response", func(b []byte) (any, error) {
		v, err := contract.DecodeLenient[contract.ErrorResponse](b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
	register("media_response", func(b []byte) (any, error) {
		v, err := contract.DecodeLenient[contract.MediaResponse](b)
		if err != nil {
			return nil, err
		}
		return v, nil
	})
}

func TestHelloMinimalDefaults(t *testing.T) {
	h, err := contract.DecodeStrict[contract.Hello](readFixture(t, "hello/minimal.json"))
	if err != nil {
		t.Fatal(err)
	}
	c := h.Capabilities
	if c == nil || c.Buttons || c.Edit || c.Typing || c.Threads || c.ReplyTo {
		t.Fatalf("booleans must default to false: %+v", c)
	}
	if len(c.MediaIn) != 0 || len(c.MediaOut) != 0 {
		t.Errorf("lists must default to empty: %+v", c)
	}
	if c.MaxText == nil || *c.MaxText != 2000 {
		t.Errorf("max_text = %v, want 2000", c.MaxText)
	}
}

func TestHelloMinimalExplicitEncodesToMinimal(t *testing.T) {
	h, err := contract.DecodeStrict[contract.Hello](readFixture(t, "hello/minimal-explicit.json"))
	if err != nil {
		t.Fatal(err)
	}
	out, _ := json.Marshal(h)
	got, _ := parseValue(out)
	want, _ := parseValue(readFixture(t, "hello/minimal.json"))
	g, _ := json.Marshal(got)
	w, _ := json.Marshal(want)
	if string(g) != string(w) {
		t.Errorf("got %s, want %s", g, w)
	}
}

func TestHelloCompat(t *testing.T) {
	for file, want := range map[string]bool{
		"hello/telegram-full.json":       true,
		"hello/compat-patch.json":        true,
		"hello/compat-newer-minor.json":  false,
		"hello/compat-future-major.json": false,
	} {
		h, err := contract.DecodeStrict[contract.Hello](readFixture(t, file))
		if err != nil {
			t.Fatalf("%s: %v", file, err)
		}
		if got := contract.Compatible(h.Version); got != want {
			t.Errorf("%s: Compatible(%q) = %v, want %v", file, h.Version, got, want)
		}
	}
}

func TestHelloResponseVersion(t *testing.T) {
	r, err := contract.DecodeLenient[contract.HelloResponse](readFixture(t, "hello/response.json"))
	if err != nil {
		t.Fatal(err)
	}
	if r.ContractVersion == nil || *r.ContractVersion != contract.Version {
		t.Errorf("contract_version = %v, want %s", r.ContractVersion, contract.Version)
	}
}

func TestHelloResponseIgnoresUnknownKeys(t *testing.T) {
	body := []byte(`{"contract_version":"1.0.0","extra":1}`)
	if _, err := contract.DecodeLenient[contract.HelloResponse](body); err != nil {
		t.Errorf("lenient decode rejected an unknown key: %v", err)
	}
}

func TestErrorResponse(t *testing.T) {
	r, err := contract.DecodeLenient[contract.ErrorResponse](readFixture(t, "errors/bad-request.json"))
	if err != nil {
		t.Fatal(err)
	}
	if *r.Error != "required_if" || r.At == nil || *r.At != "/conversation/parent" {
		t.Errorf("unexpected body: %+v", r)
	}
	r, err = contract.DecodeLenient[contract.ErrorResponse](readFixture(t, "errors/contract-version.json"))
	if err != nil {
		t.Fatal(err)
	}
	if *r.Error != contract.ErrContractVersion || r.ContractVersion != contract.Version {
		t.Errorf("unexpected body: %+v", r)
	}
	r, err = contract.DecodeLenient[contract.ErrorResponse]([]byte(`{"error":"grammar","at":""}`))
	if err != nil || r.At == nil || *r.At != "" {
		t.Fatalf("at empty must stay distinct from absent: %+v %v", r, err)
	}
	out, _ := json.Marshal(r)
	if string(out) != `{"error":"grammar","at":""}` {
		t.Errorf("re-encoded %s", out)
	}
	for file, want := range map[string]string{
		"errors/channel-mismatch.json": contract.ErrChannelMismatch,
		"errors/media-too-large.json":  contract.ErrMediaTooLarge,
		"errors/not-leased.json":       contract.ErrNotLeased,
	} {
		r, err := contract.DecodeLenient[contract.ErrorResponse](readFixture(t, file))
		if err != nil || *r.Error != want {
			t.Errorf("%s: %+v %v", file, r, err)
		}
	}
}

func TestMediaResponse(t *testing.T) {
	r, err := contract.DecodeLenient[contract.MediaResponse](readFixture(t, "media/upload-response.json"))
	if err != nil {
		t.Fatal(err)
	}
	if r.MediaID == nil || *r.MediaID != "m_17" {
		t.Errorf("media_id = %v", r.MediaID)
	}
}
