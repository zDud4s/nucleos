module nucleosremote

go 1.26

// Pinned, not left to whatever the machine has: this process is the only one in NucleOS that faces
// a network other than loopback (through `tailscale serve`), so the stdlib it is built with is its
// exposure. Same pin as the other sidecars.
toolchain go1.26.6
