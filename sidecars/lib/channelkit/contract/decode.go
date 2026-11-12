package contract

import (
	"bytes"
	"encoding/json"
	"io"
	"reflect"
	"strconv"
	"strings"
)

// DecodeStrict decodes an input shape: duplicate keys, unknown keys and keys with
// a different case are all "decode" errors. Required fields (tag `kit:"required"`
// on a pointer, slice, map or interface field) must be present. Validate runs last.
func DecodeStrict[T any, P interface {
	*T
	Validate() *Error
}](b []byte) (T, error) {
	return decode[T, P](b, true)
}

// DecodeLenient decodes an output shape: unknown keys and keys whose case differs
// are ignored, but duplicate keys, malformed JSON, wrong types and missing
// required fields are still "decode" errors.
func DecodeLenient[T any, P interface {
	*T
	Validate() *Error
}](b []byte) (T, error) {
	return decode[T, P](b, false)
}

func decode[T any, P interface {
	*T
	Validate() *Error
}](b []byte, strict bool) (T, error) {
	var zero, v T
	if err := noDuplicateKeys(b); err != nil {
		return zero, err
	}
	dec := json.NewDecoder(bytes.NewReader(b))
	dec.UseNumber()
	var tree any
	if err := dec.Decode(&tree); err != nil {
		return zero, &Error{Code: CodeDecode}
	}
	if _, err := dec.Token(); err != io.EOF {
		return zero, &Error{Code: CodeDecode}
	}
	filtered, derr := filterKeys(tree, reflect.TypeOf(v), "", strict)
	if derr != nil {
		return zero, derr
	}
	raw, err := json.Marshal(filtered)
	if err != nil {
		return zero, &Error{Code: CodeDecode}
	}
	d2 := json.NewDecoder(bytes.NewReader(raw))
	d2.UseNumber()
	if err := d2.Decode(&v); err != nil {
		return zero, &Error{Code: CodeDecode}
	}
	if at, ok := missingRequired(reflect.ValueOf(&v).Elem(), ""); !ok {
		return zero, &Error{Code: CodeDecode, At: at}
	}
	if verr := P(&v).Validate(); verr != nil {
		return zero, verr
	}
	return v, nil
}

// noDuplicateKeys walks the token stream and rejects an object that repeats a key.
func noDuplicateKeys(b []byte) error {
	dec := json.NewDecoder(bytes.NewReader(b))
	dec.UseNumber()
	var walk func() error
	walk = func() error {
		t, err := dec.Token()
		if err != nil {
			return &Error{Code: CodeDecode}
		}
		d, ok := t.(json.Delim)
		if !ok {
			return nil
		}
		if d == '[' {
			for dec.More() {
				if err := walk(); err != nil {
					return err
				}
			}
		} else {
			seen := map[string]bool{}
			for dec.More() {
				k, err := dec.Token()
				if err != nil {
					return &Error{Code: CodeDecode}
				}
				ks, _ := k.(string)
				if seen[ks] {
					return &Error{Code: CodeDecode}
				}
				seen[ks] = true
				if err := walk(); err != nil {
					return err
				}
			}
		}
		if _, err := dec.Token(); err != nil {
			return &Error{Code: CodeDecode}
		}
		return nil
	}
	return walk()
}

var unmarshalerType = reflect.TypeOf((*json.Unmarshaler)(nil)).Elem()

// jsonName returns the JSON key of a struct field, or "" when it is skipped.
func jsonName(f reflect.StructField) string {
	if !f.IsExported() {
		return ""
	}
	name, _, _ := strings.Cut(f.Tag.Get("json"), ",")
	if name == "-" {
		return ""
	}
	if name == "" {
		name = f.Name
	}
	return name
}

// filterKeys checks (strict) or drops (lenient) object keys that are not an exact
// match of a json tag name of the target struct, at every level. The valid keys
// come from the struct tags, so an omitempty key present in the input is fine.
func filterKeys(v any, t reflect.Type, at string, strict bool) (any, *Error) {
	for t.Kind() == reflect.Ptr {
		t = t.Elem()
	}
	if reflect.PointerTo(t).Implements(unmarshalerType) {
		return v, nil
	}
	switch x := v.(type) {
	case map[string]any:
		switch t.Kind() {
		case reflect.Struct:
			fields := map[string]reflect.Type{}
			for i := 0; i < t.NumField(); i++ {
				if n := jsonName(t.Field(i)); n != "" {
					fields[n] = t.Field(i).Type
				}
			}
			out := make(map[string]any, len(x))
			for k, val := range x {
				ft, ok := fields[k]
				if !ok {
					if strict {
						return nil, &Error{Code: CodeDecode, At: at + "/" + escape(k)}
					}
					continue
				}
				fv, err := filterKeys(val, ft, at+"/"+escape(k), strict)
				if err != nil {
					return nil, err
				}
				out[k] = fv
			}
			return out, nil
		case reflect.Map:
			out := make(map[string]any, len(x))
			for k, val := range x {
				fv, err := filterKeys(val, t.Elem(), at+"/"+escape(k), strict)
				if err != nil {
					return nil, err
				}
				out[k] = fv
			}
			return out, nil
		}
	case []any:
		if t.Kind() == reflect.Slice || t.Kind() == reflect.Array {
			out := make([]any, len(x))
			for i, val := range x {
				fv, err := filterKeys(val, t.Elem(), at+"/"+strconv.Itoa(i), strict)
				if err != nil {
					return nil, err
				}
				out[i] = fv
			}
			return out, nil
		}
	}
	return v, nil
}

func escape(k string) string {
	return strings.NewReplacer("~", "~0", "/", "~1").Replace(k)
}

func nilable(k reflect.Kind) bool {
	switch k {
	case reflect.Ptr, reflect.Slice, reflect.Map, reflect.Interface:
		return true
	}
	return false
}

// missingRequired reports the pointer of the first `kit:"required"` field that is
// absent (nil). The second result is false when one is missing.
func missingRequired(v reflect.Value, at string) (string, bool) {
	switch v.Kind() {
	case reflect.Ptr, reflect.Interface:
		if v.IsNil() {
			return "", true
		}
		return missingRequired(v.Elem(), at)
	case reflect.Slice, reflect.Array:
		for i := 0; i < v.Len(); i++ {
			if p, ok := missingRequired(v.Index(i), at+"/"+strconv.Itoa(i)); !ok {
				return p, false
			}
		}
	case reflect.Map:
		for _, k := range v.MapKeys() {
			if p, ok := missingRequired(v.MapIndex(k), at+"/"+escape(k.String())); !ok {
				return p, false
			}
		}
	case reflect.Struct:
		t := v.Type()
		for i := 0; i < t.NumField(); i++ {
			f := t.Field(i)
			name := jsonName(f)
			if name == "" {
				continue
			}
			fv := v.Field(i)
			p := at + "/" + escape(name)
			if f.Tag.Get("kit") == "required" && nilable(f.Type.Kind()) && fv.IsNil() {
				return p, false
			}
			if q, ok := missingRequired(fv, p); !ok {
				return q, false
			}
		}
	}
	return "", true
}
