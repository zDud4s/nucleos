package send

import (
	"bufio"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"nucleosemail/config"
)

// fakeSMTP speaks just enough SMTP to accept a message, and records every RCPT TO.
func fakeSMTP(t *testing.T, silent bool) (addr string, rcpts func() []string) {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	var mu sync.Mutex
	var got []string
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		if silent {
			time.Sleep(5 * time.Second)
			return
		}
		r := bufio.NewReader(conn)
		say := func(s string) { _, _ = conn.Write([]byte(s + "\r\n")) }
		say("220 fake")
		inData := false
		for {
			line, err := r.ReadString('\n')
			if err != nil {
				return
			}
			line = strings.TrimRight(line, "\r\n")
			if inData {
				if line == "." {
					inData = false
					say("250 queued")
				}
				continue
			}
			switch strings.ToUpper(strings.Fields(line + " x")[0]) {
			case "EHLO":
				say("250-fake")
				say("250 AUTH PLAIN")
			case "AUTH":
				say("235 ok")
			case "MAIL":
				say("250 ok")
			case "RCPT":
				mu.Lock()
				got = append(got, line)
				mu.Unlock()
				say("250 ok")
			case "DATA":
				inData = true
				say("354 go")
			case "QUIT":
				say("221 bye")
				return
			default:
				say("250 ok")
			}
		}
	}()
	return listener.Addr().String(), func() []string {
		mu.Lock()
		defer mu.Unlock()
		return append([]string(nil), got...)
	}
}

func TestEveryRecipientGetsAnRcptTo(t *testing.T) {
	addr, rcpts := fakeSMTP(t, false)
	conn, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	recipients, err := Recipients("bruno@example.com, Carla <carla@example.com>")
	if err != nil {
		t.Fatal(err)
	}
	cfg := config.Config{Username: "ana@example.com", Password: "pw", SMTPHost: "127.0.0.1"}
	if err := submit(conn, cfg, recipients, []byte("Subject: hi\r\n\r\nbody")); err != nil {
		t.Fatalf("submit: %v", err)
	}
	got := rcpts()
	if len(got) != 2 || !strings.Contains(got[0], "bruno@example.com") || !strings.Contains(got[1], "carla@example.com") {
		t.Fatalf("RCPT lines = %q, want both recipients", got)
	}
}

func TestAStalledSubmissionServerTimesOut(t *testing.T) {
	addr, _ := fakeSMTP(t, true)
	conn, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	old := sessionTimeout
	sessionTimeout = 200 * time.Millisecond
	defer func() { sessionTimeout = old }()

	done := make(chan error, 1)
	go func() {
		cfg := config.Config{Username: "a@example.com", SMTPHost: "127.0.0.1"}
		done <- submit(conn, cfg, []string{"b@example.com"}, []byte("x"))
	}()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("submit succeeded against a silent server")
		}
	case <-time.After(3 * time.Second):
		t.Fatal("submit hung on a stalled server: no deadline")
	}
}

func TestANonASCIISubjectIsEncodedAndRecipientsAreValidated(t *testing.T) {
	framed, err := Build("ana@example.com", "", Message{To: "b@example.com", Subject: "Orçamento", Body: "x"}, time.Now(), "id")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(framed), "Subject: =?utf-8?q?Or=C3=A7amento?=\r\n") {
		t.Fatalf("subject not RFC 2047 encoded:\n%s", framed)
	}
	if _, err := Build("ana@example.com", "", Message{To: "not an address", Subject: "s"}, time.Now(), "id"); err == nil {
		t.Fatal("an unparseable To was accepted")
	}
}

func TestAnOversizedRequestBodyIsRefused(t *testing.T) {
	cfg := config.Config{SMTPHost: "smtp.example.com", SMTPPort: 465}
	big := `{"to":"b@example.com","subject":"s","body":"` + strings.Repeat("a", maxRequestBody+10) + `"}`
	rec := httptest.NewRecorder()
	Handler(cfg)(rec, httptest.NewRequest(http.MethodPost, "/send", strings.NewReader(big)))
	if rec.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("answered %d, want 413", rec.Code)
	}
}
