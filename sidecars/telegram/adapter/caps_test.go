package adapter

import (
	"encoding/json"
	"io/fs"
	"reflect"
	"testing"

	"nucleoschannelkit/contract"
	"nucleoschannelkit/contract/fixtures"
)

func helloFixture(t *testing.T) []byte {
	t.Helper()
	b, err := fs.ReadFile(fixtures.FS(), "hello/telegram-full.json")
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func TestCapsMatchSharedFixture(t *testing.T) {
	want, err := contract.DecodeStrict[contract.Hello](helloFixture(t))
	if err != nil {
		t.Fatal(err)
	}
	got := Capabilities()
	if !reflect.DeepEqual(got, *want.Capabilities) {
		t.Fatalf("capabilities differ from fixture:\n got %+v\nwant %+v", got, *want.Capabilities)
	}
	if got.MaxText == nil || *got.MaxText != 4096 {
		t.Fatalf("max_text = %v, want 4096", got.MaxText)
	}
}

func TestCapsHello(t *testing.T) {
	h := Hello()
	if h.Kind != "telegram" || h.Version != contract.Version {
		t.Fatalf("hello = %q %q", h.Kind, h.Version)
	}
	b, err := json.Marshal(h)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := contract.DecodeStrict[contract.Hello](b); err != nil {
		t.Fatalf("DecodeStrict: %v", err)
	}
	want, _ := contract.DecodeStrict[contract.Hello](helloFixture(t))
	if !reflect.DeepEqual(h, want) {
		t.Fatalf("hello differs from fixture")
	}
}
