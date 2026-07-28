package imap

import (
	"bufio"
	"fmt"
	"net"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2/imapclient"
)

// Enough of an IMAP server to answer the four commands this package sends, and — the whole reason
// it exists — a recorder of every line that actually crossed the wire.
//
// The source scan in readonly_test.go proves we never WRITE a mutating call. It cannot prove the
// library does not send one on our behalf, and it cannot prove that `ReadOnly: true` and
// `Peek: true` become EXAMINE and BODY.PEEK rather than being quietly ignored by a version bump.
// Those are claims about bytes, so they are tested against bytes.
type fakeServer struct {
	listener net.Listener

	mu       sync.Mutex
	received []string

	// What the mailbox answers with. Set before a client connects.
	uidValidity uint32
	searchUIDs  []uint32
	message     []byte
	internal    time.Time
}

const fakeInternalDateLayout = "02-Jan-2006 15:04:05 -0700"

func newFakeServer(t *testing.T) *fakeServer {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("could not listen: %v", err)
	}
	server := &fakeServer{
		listener:    listener,
		uidValidity: 42,
		searchUIDs:  []uint32{5, 6, 7},
		message:     []byte("Subject: hello\r\n\r\nbody text\r\n"),
		internal:    time.Date(2026, 7, 27, 18, 15, 42, 0, time.UTC),
	}
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go server.serve(conn)
		}
	}()
	t.Cleanup(func() { _ = listener.Close() })
	return server
}

// connect builds a Conn around a client speaking to this server. It bypasses Dial because Dial
// insists on TLS against the real world; everything below Dial is what these tests are about, and
// the test lives in this package precisely so it can reach it.
func (s *fakeServer) connect(t *testing.T) *Conn {
	t.Helper()
	netConn, err := net.Dial("tcp", s.listener.Addr().String())
	if err != nil {
		t.Fatalf("could not dial the fake server: %v", err)
	}
	client := imapclient.New(netConn, nil)
	if err := client.WaitGreeting(); err != nil {
		t.Fatalf("no greeting: %v", err)
	}
	conn := &Conn{client: client}
	t.Cleanup(conn.Close)
	return conn
}

func (s *fakeServer) record(line string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.received = append(s.received, line)
}

// lines returns everything the client sent, uppercased — every assertion here is about which verb
// was used, never about the tag or the casing the encoder happened to pick.
func (s *fakeServer) lines() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]string, 0, len(s.received))
	for _, line := range s.received {
		out = append(out, strings.ToUpper(line))
	}
	return out
}

func (s *fakeServer) sent(t *testing.T, needle string) string {
	t.Helper()
	for _, line := range s.lines() {
		if strings.Contains(line, strings.ToUpper(needle)) {
			return line
		}
	}
	t.Fatalf("no command containing %q was sent; got %v", needle, s.lines())
	return ""
}

func (s *fakeServer) serve(conn net.Conn) {
	defer conn.Close()
	reader := bufio.NewReader(conn)
	writer := bufio.NewWriter(conn)

	// Only IMAP4rev1, so SEARCH answers come back in the untagged form this fake speaks.
	fmt.Fprint(writer, "* OK [CAPABILITY IMAP4rev1] fake ready\r\n")
	_ = writer.Flush()

	for {
		line, err := reader.ReadString('\n')
		if err != nil {
			return
		}
		line = strings.TrimRight(line, "\r\n")
		s.record(line)

		tag, rest, _ := strings.Cut(line, " ")
		command := strings.ToUpper(rest)

		switch {
		case strings.HasPrefix(command, "CAPABILITY"):
			fmt.Fprintf(writer, "* CAPABILITY IMAP4rev1\r\n%s OK done\r\n", tag)
		case strings.HasPrefix(command, "LOGIN"):
			fmt.Fprintf(writer, "%s OK done\r\n", tag)
		case strings.HasPrefix(command, "EXAMINE"), strings.HasPrefix(command, "SELECT"):
			fmt.Fprintf(writer, "* %d EXISTS\r\n", len(s.searchUIDs))
			fmt.Fprintf(writer, "* OK [UIDVALIDITY %d] UIDs valid\r\n", s.uidValidity)
			fmt.Fprintf(writer, "%s OK [READ-ONLY] done\r\n", tag)
		case strings.HasPrefix(command, "UID SEARCH"):
			parts := make([]string, 0, len(s.searchUIDs))
			for _, uid := range s.searchUIDs {
				parts = append(parts, fmt.Sprint(uid))
			}
			if len(parts) > 0 {
				fmt.Fprintf(writer, "* SEARCH %s\r\n", strings.Join(parts, " "))
			} else {
				fmt.Fprint(writer, "* SEARCH\r\n")
			}
			fmt.Fprintf(writer, "%s OK done\r\n", tag)
		case strings.HasPrefix(command, "UID FETCH"):
			uid := s.searchUIDs[0]
			fmt.Fprintf(writer, "* 1 FETCH (UID %d INTERNALDATE \"%s\" BODY[] {%d}\r\n",
				uid, s.internal.Format(fakeInternalDateLayout), len(s.message))
			writer.Write(s.message)
			fmt.Fprint(writer, ")\r\n")
			fmt.Fprintf(writer, "%s OK done\r\n", tag)
		case strings.HasPrefix(command, "LOGOUT"):
			fmt.Fprintf(writer, "* BYE\r\n%s OK done\r\n", tag)
			_ = writer.Flush()
			return
		default:
			fmt.Fprintf(writer, "%s BAD unsupported in the fake: %s\r\n", tag, command)
		}
		_ = writer.Flush()
	}
}
