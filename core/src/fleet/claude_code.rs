//! Claude Code as a fleet source: sessions registry + transcripts + subagents + daemon-run roots.
//! Spec `.ai/specs/2026-10-20-fleet-arvore-viva-design.md` §3-§4.

mod record;
mod session;
#[cfg(test)]
mod fixture;
