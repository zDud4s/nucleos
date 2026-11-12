package imap

import (
	"net"
	"testing"
	"time"
)

// A server that accepts and then says nothing must make a command fail, not hang the poll loop.
func TestAStalledServerTimesOutInsteadOfHanging(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	go func() {
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		time.Sleep(5 * time.Second) // never speaks
	}()

	netConn, err := net.Dial("tcp", listener.Addr().String())
	if err != nil {
		t.Fatal(err)
	}
	conn := newConn(netConn, 200*time.Millisecond)
	defer conn.Close()

	done := make(chan error, 1)
	go func() {
		_, err := conn.Select("INBOX")
		done <- err
	}()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("Select against a silent server succeeded")
		}
	case <-time.After(3 * time.Second):
		t.Fatal("Select hung on a stalled server: no I/O deadline")
	}
}
