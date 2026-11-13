// Command winres writes a Windows COFF object (.syso) carrying a VERSION resource, so the Go
// linker embeds FileDescription/ProductName/... into a sidecar exe. It has no dependencies:
// `scripts/build-sidecars.sh` runs it with `go run .` when GOOS=windows.
//
// Usage: winres -name <sidecar> -version <x.y.z> -arch <GOARCH> -out <file.syso>
package main

import (
	"bytes"
	"encoding/binary"
	"flag"
	"fmt"
	"os"
	"strconv"
	"strings"
	"unicode/utf16"
	"unicode/utf8"
)

const (
	companyName = "NucleOS" // keep in sync with `publisher` in shell/src-tauri/tauri.conf.json
	productName = "NucleOS"
)

// Role turns a sidecar directory name into its display role: "email" -> "Email".
func Role(name string) string {
	if name == "" {
		return ""
	}
	r, n := utf8.DecodeRuneInString(name)
	return strings.ToUpper(string(r)) + name[n:]
}

// Description is the FileDescription shown by the Task Manager: "NucleOS — Email" (U+2014).
func Description(name string) string { return "NucleOS — " + Role(name) }

// ParseVersion reads "x.y.z[-pre]" into four 16-bit parts (missing parts are 0).
func ParseVersion(v string) ([4]uint16, error) {
	var out [4]uint16
	core := strings.SplitN(strings.SplitN(v, "-", 2)[0], "+", 2)[0]
	parts := strings.Split(core, ".")
	if len(parts) > 4 {
		return out, fmt.Errorf("bad version %q", v)
	}
	for i, p := range parts {
		n, err := strconv.ParseUint(p, 10, 16)
		if err != nil {
			return out, fmt.Errorf("bad version %q: %w", v, err)
		}
		out[i] = uint16(n)
	}
	return out, nil
}

func u16(s string) []byte {
	var b bytes.Buffer
	for _, c := range utf16.Encode([]rune(s)) {
		binary.Write(&b, binary.LittleEndian, c)
	}
	binary.Write(&b, binary.LittleEndian, uint16(0))
	return b.Bytes()
}

func pad4(b []byte) []byte {
	for len(b)%4 != 0 {
		b = append(b, 0)
	}
	return b
}

// node encodes a version-info block: header, key, value, then children (each 4-aligned).
// wLength covers the block without its own trailing padding.
func node(key string, valueLen uint16, typ uint16, value []byte, children ...[]byte) []byte {
	body := pad4(append([]byte{0, 0, 0, 0, 0, 0}, u16(key)...))
	body = append(body, value...)
	for _, c := range children {
		body = pad4(body)
		body = append(body, c...)
	}
	binary.LittleEndian.PutUint16(body[0:], uint16(len(body)))
	binary.LittleEndian.PutUint16(body[2:], valueLen)
	binary.LittleEndian.PutUint16(body[4:], typ)
	return body
}

func le32(vs ...uint32) []byte {
	var b bytes.Buffer
	for _, v := range vs {
		binary.Write(&b, binary.LittleEndian, v)
	}
	return b.Bytes()
}

// VersionInfo builds the RT_VERSION payload for one sidecar.
func VersionInfo(name, version string) ([]byte, error) {
	v, err := ParseVersion(version)
	if err != nil {
		return nil, err
	}
	ms := uint32(v[0])<<16 | uint32(v[1])
	ls := uint32(v[2])<<16 | uint32(v[3])
	fixed := le32(0xFEEF04BD, 0x00010000, ms, ls, ms, ls, 0x3F, 0, 0x00040004, 1, 0, 0, 0)
	strs := [][2]string{
		{"CompanyName", companyName},
		{"FileDescription", Description(name)},
		{"FileVersion", version},
		{"InternalName", "nucleos-" + name},
		{"OriginalFilename", "nucleos-" + name + ".exe"},
		{"ProductName", productName},
		{"ProductVersion", version},
	}
	var entries [][]byte
	for _, kv := range strs {
		val := u16(kv[1])
		entries = append(entries, node(kv[0], uint16(len(val)/2), 1, val))
	}
	table := node("040904B0", 0, 1, nil, entries...)
	sfi := node("StringFileInfo", 0, 1, nil, table)
	translation := node("Translation", 4, 0, le32(0x04B00409))
	vfi := node("VarFileInfo", 0, 1, nil, translation)
	return node("VS_VERSION_INFO", uint16(len(fixed)), 0, fixed, sfi, vfi), nil
}

// Syso wraps a version payload in a one-section COFF object for the given GOARCH.
func Syso(arch string, payload []byte) ([]byte, error) {
	var machine, reloc uint16
	switch arch {
	case "amd64":
		machine, reloc = 0x8664, 3 // IMAGE_REL_AMD64_ADDR32NB
	case "386":
		machine, reloc = 0x14c, 7 // IMAGE_REL_I386_DIR32NB
	case "arm64":
		machine, reloc = 0xAA64, 2 // IMAGE_REL_ARM64_ADDR32NB
	default:
		return nil, fmt.Errorf("unsupported GOARCH %q", arch)
	}
	// Resource tree: type 16 (VERSION) -> id 1 -> lang 0x0409 -> data entry -> payload.
	var sec bytes.Buffer
	dir := func(id, next uint32, sub bool) {
		sec.Write(le32(0, 0))            // characteristics, timestamp
		sec.Write([]byte{0, 0, 0, 0})    // version
		sec.Write([]byte{0, 0, 1, 0})    // 0 named entries, 1 id entry
		if sub {
			next |= 0x80000000
		}
		sec.Write(le32(id, next))
	}
	dir(16, 24, true)
	dir(1, 48, true)
	dir(0x0409, 72, false)
	const dataOff = 88
	sec.Write(le32(dataOff, uint32(len(payload)), 0, 0)) // data entry at 72
	sec.Write(payload)
	raw := pad4(sec.Bytes())

	var out bytes.Buffer
	const hdr, shdr = 20, 40
	relocOff := uint32(hdr + shdr + len(raw))
	symOff := relocOff + 10
	w := func(v any) { binary.Write(&out, binary.LittleEndian, v) }
	w(machine)
	w(uint16(1))
	w(uint32(0))
	w(symOff)
	w(uint32(1))
	w(uint16(0))
	w(uint16(0))
	out.WriteString(".rsrc\x00\x00\x00")
	w(uint32(0))
	w(uint32(0))
	w(uint32(len(raw)))
	w(uint32(hdr + shdr))
	w(relocOff)
	w(uint32(0))
	w(uint16(1))
	w(uint16(0))
	w(uint32(0x40000040))
	out.Write(raw)
	w(uint32(72)) // relocation: RVA field of the data entry
	w(uint32(0))  // symbol 0
	w(reloc)
	out.WriteString(".rsrc\x00\x00\x00") // symbol 0: section symbol
	w(uint32(0))
	w(uint16(1))
	w(uint16(0))
	w(uint8(3))
	w(uint8(0))
	w(uint32(4)) // empty string table
	return out.Bytes(), nil
}

func main() {
	name := flag.String("name", "", "sidecar directory name")
	version := flag.String("version", "", "version x.y.z")
	arch := flag.String("arch", "amd64", "GOARCH")
	outPath := flag.String("out", "", "output .syso")
	flag.Parse()
	if *name == "" || *version == "" || *outPath == "" {
		flag.Usage()
		os.Exit(2)
	}
	payload, err := VersionInfo(*name, *version)
	if err == nil {
		var obj []byte
		if obj, err = Syso(*arch, payload); err == nil {
			err = os.WriteFile(*outPath, obj, 0o644)
		}
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "winres:", err)
		os.Exit(1)
	}
}
