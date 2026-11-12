// §spec browser-com-painel

// Package panelui carries the panel bundle the driver injects into every target of a visible session.
package panelui

import _ "embed"

// Source is the panel bundle, a single script.
//
//go:embed panel.js
var Source string
