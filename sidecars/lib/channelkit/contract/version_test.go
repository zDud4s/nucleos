package contract

import "testing"

func TestVersionConstant(t *testing.T) {
	if Version != "1.0.0" {
		t.Fatalf("Version = %q, want 1.0.0", Version)
	}
}

func TestCompatible(t *testing.T) {
	cases := []struct {
		adapter string
		want    bool
	}{
		{"1.0.9", true},
		{"1.0.0", true},
		{"1.1.0", false},
		{"2.0.0", false},
		{"1.0", false},
		{"v1", false},
		{"", false},
	}
	for _, c := range cases {
		if got := Compatible(c.adapter); got != c.want {
			t.Errorf("Compatible(%q) = %v, want %v", c.adapter, got, c.want)
		}
	}
}
