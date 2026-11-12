// §spec browser-ao-vivo

package serve

import (
	"encoding/binary"
	"fmt"
	"io"
)

// The records /watch streams. One kind byte, a four-byte big-endian length, then that many bytes.
//
// A record stream and not multipart or SSE because the body is a JPEG: there is nothing to escape, and
// a length prefix is the one framing a reader can trust without scanning for a boundary the picture
// might contain.
const (
	// RecordFrame carries one JPEG.
	RecordFrame byte = 'F'
	// RecordEnd carries `{"reason":"closed|wheel|gone"}` and is the last record of a stream.
	RecordEnd byte = 'E'
	// RecordMeta carries the frame geometry as JSON; it precedes the first frame and any frame whose
	// geometry differs from the last one written.
	RecordMeta byte = 'M'
	// RecordPrompt carries a prompt for the viewer to answer, as JSON.
	RecordPrompt byte = 'P'
	// RecordPanel carries one event of the panel channel (/panel/events), as JSON.
	RecordPanel byte = 'N'
)

// MaxRecord bounds what ReadRecord will allocate for one record.
const MaxRecord = 16 << 20

// WriteRecord writes one record: the header, then the body.
func WriteRecord(w io.Writer, kind byte, body []byte) error {
	var header [5]byte
	header[0] = kind
	binary.BigEndian.PutUint32(header[1:], uint32(len(body)))
	if _, err := w.Write(header[:]); err != nil {
		return err
	}
	if len(body) == 0 {
		return nil
	}
	_, err := w.Write(body)
	return err
}

// ReadRecord reads one record. It returns io.EOF only between records, and refuses a length over
// MaxRecord before allocating anything for it.
func ReadRecord(r io.Reader) (kind byte, body []byte, err error) {
	var header [5]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return 0, nil, err
	}
	length := binary.BigEndian.Uint32(header[1:])
	if length > MaxRecord {
		return 0, nil, fmt.Errorf("serve: a %d-byte record is over the %d-byte limit", length, MaxRecord)
	}
	body = make([]byte, length)
	if _, err := io.ReadFull(r, body); err != nil {
		if err == io.EOF {
			err = io.ErrUnexpectedEOF
		}
		return 0, nil, err
	}
	return header[0], body, nil
}
