//! The workflow library, the pin a project keeps, and how far the two have drifted apart.
//!
//! A workflow is not a file. §6.1 of the design counts five pieces — the graph, the node bodies,
//! the contracts, the scripts and the model assignments — and this repository's own `.ai/` harness
//! has all five, spread across a dozen files that never say they are one thing. So a **bundle** is
//! a directory, and the library is a directory of them:
//!
//! ```text
//! ~/.nucleos/workflows/<name>/<version>/
//! ```
//!
//! **`~/.nucleos` and not the app's data directory, deliberately.** Everything else this daemon
//! keeps for itself goes through `directories::ProjectDirs` — a path on Windows nobody types by
//! accident, which is right for a database. A workflow bundle is the opposite kind of thing: it is
//! meant to be opened, read, edited and copied between machines by a person, and §6.3's second exit
//! (`edit it in the library`) is worthless if the library is somewhere only the app can find. The
//! design named the path; this follows it rather than improving on it.
//!
//! # The hash is over bytes, and that is what keeps this slice honest
//!
//! The plan's open decision C says the graph's serialisation format belongs to the second spec, and
//! that *"if slice 7 needs to fix it first, the boundary between the specs is in the wrong place"*.
//! It does not need to. Everything here — list, install, eject, drift — is answered by hashing the
//! bundle's files and comparing hashes. The only file this module ever opens and *parses* is
//! [`MANIFEST`], which carries a name, a version, an origin and the files the workflow authors in a
//! project. What a node is, what an edge is, how a condition is written: none of it is read here.
//!
//! That is not an evasion, it is the property that makes the boundary real. A canvas can arrive
//! later and invent whatever format it likes without a single change to installation or drift.
//!
//! # The pin carries origin and hash, not a version number
//!
//! §6.1's first named weakness: a new machine has to be able to materialise the bundle that is
//! missing, instead of ending up with no workflow at all. The repository has already been bitten by
//! exactly this — `CLAUDE.md` records that `.githooks/` does not travel with the repo and that a
//! fresh clone must recreate it "or the guard is inert".
//!
//! So [`Pin`] records where the bundle came from and what it hashed to. This slice does not *fetch*
//! — there is no publication format yet, and §14 says there will not be one until the second spec —
//! but recording a coordinate nothing reads would be the `.githooks` mistake told a second time.
//! Both fields are read here, on every listing: the hash is what separates `referenced` from
//! `drifted`, and the origin is what a person is shown when the library has nothing under that name
//! and the answer has to be *this is what you are missing* rather than *no workflow*.
//!
//! # Four standings, and none of them collapses into another
//!
//! §12 requires `referenced` ≠ `ejected` ≠ `drifted`. There is a fourth, and it is the one a new
//! machine sees:
//!
//! - **`referenced`** — the library has the pinned bundle and it hashes to what was pinned.
//! - **`drifted`** — the library has it, and it does not. Somebody edited the bundle in place under
//!   a pin that claims to know what it says.
//! - **`ejected`** — this project has its own copy under `.ai/workflows/<name>/`. It is never
//!   `missing`, whatever the library holds: the project has the bundle in its hands.
//! - **`missing`** — pinned, and the library has nothing at that coordinate.
//!
//! A newer *version* sitting in the library beside the pinned one is none of these. It is an offer,
//! reported separately as `update_available`, because "somebody published 1.1" and "the 1.0 you
//! pinned is not the 1.0 you have" are opposite facts that would send a person to opposite places.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where a project records which workflows it uses. Relative to the project root, forward slashes.
pub const PINS_PATH: &str = ".ai/workflows.yaml";

/// Where an ejected bundle's copy lands, per §6.1. One directory per workflow name.
pub const EJECTED_DIR: &str = ".ai/workflows";

/// The one file in a bundle this module parses. Everything else is bytes to be hashed.
pub const MANIFEST: &str = "bundle.yaml";

/// How deep a bundle is walked, how many files are read, and how many bytes.
///
/// The same floor `commands.rs` puts under its own walk, for the same reason: a bundle is a handful
/// of markdown files and a script or two, and a walk with no ceiling is a walk that somebody's
/// stray `node_modules` inside the library turns into a stall on a page that polls.
const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 4_000;
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The library on this machine, or `None` when there is no home directory to hang it off.
pub fn library_root() -> Option<PathBuf> {
    crate::commands::home().map(|home| home.join(".nucleos").join("workflows"))
}

/* ------------------------------------------------------------------ names -- */

/// Whether `name` may be used as a directory under the library or under `.ai/workflows/`.
///
/// Deliberately stricter than the filesystem. This name is joined onto a path **and** used to
/// address a route, and it is the argument to the one operation in this module that deletes a
/// directory tree. A dot-only name, a separator, a drive letter or a leading dash each turn one of
/// those three into something other than what it reads as, so the set is closed rather than
/// filtered: letters, digits, dash, underscore and dot, never starting with a dot.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.starts_with('-')
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
}

/* ---------------------------------------------------------------- hashing -- */

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Every file under `dir`, by relative forward-slash path, each mapped to the hash of its contents.
///
/// Sorted by construction — a `BTreeMap` — because the bundle hash below folds this in order and a
/// hash that depended on the order a directory happened to be read in would report drift on a
/// machine that had merely defragmented.
pub fn file_hashes(dir: &Path) -> std::io::Result<BTreeMap<String, String>> {
    let mut found = BTreeMap::new();
    let mut budget = MAX_BYTES;
    walk(dir, "", 0, &mut found, &mut budget)?;
    Ok(found)
}

fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    found: &mut BTreeMap<String, String>,
    budget: &mut u64,
) -> std::io::Result<()> {
    if depth > MAX_DEPTH || found.len() >= MAX_FILES {
        return Ok(());
    }
    // Sorted so the walk is deterministic even before the map is: with a ceiling on files, WHICH
    // files get read has to be a decision rather than an accident of the directory order.
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if found.len() >= MAX_FILES {
            return Ok(());
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        // `file_type` and not `metadata`: a symlink into somewhere enormous is followed by the
        // second and reported as itself by the first. A bundle that linked out of the library would
        // otherwise be hashed against a tree nobody meant to publish.
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(&entry.path(), &rel, depth + 1, found, budget)?;
            continue;
        }
        let contents = std::fs::read(entry.path())?;
        let size = contents.len() as u64;
        if size > *budget {
            return Ok(());
        }
        *budget -= size;
        found.insert(rel, hex(&Sha256::digest(&contents)));
    }
    Ok(())
}

/// One hash for a whole bundle, `sha256:…`.
///
/// Folds the per-file hashes with their paths, each length-prefixed. Without the lengths, a file
/// called `ab` beside a file called `c` would hash the same as a file called `a` beside one called
/// `bc` — a collision cheap enough to hit by accident and exactly the kind of thing a "your bundle
/// has not changed" claim must not be able to get wrong.
pub fn digest_of(files: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (path, hash) in files {
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update((hash.len() as u64).to_le_bytes());
        hasher.update(hash.as_bytes());
    }
    format!("sha256:{}", hex(&hasher.finalize()))
}

/* ----------------------------------------------------------------- bundle -- */

#[derive(Debug, Default, Deserialize)]
struct ManifestFile {
    description: Option<String>,
    /// Where this bundle came from, for a machine that does not have it. Free text on purpose:
    /// there is no publication format yet (§14), and inventing a URL scheme this slice cannot
    /// resolve would be worse than recording what the author wrote.
    origin: Option<String>,
    /// Files in a project this workflow is the author of. See [`crate::ownership`].
    #[serde(default)]
    owns: Vec<String>,
}

/// One bundle in the library, as it is on disk right now.
#[derive(Debug, Clone, Serialize)]
pub struct Bundle {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub origin: String,
    pub owns: Vec<String>,
    pub hash: String,
    /// Absolute, for the "open it in the editor" door. The shell never joins onto it.
    pub path: String,
}

/// Read one bundle directory. `None` when it has no manifest — which is not an error.
///
/// A directory under the library with no `bundle.yaml` is somebody's scratch folder, a half-finished
/// checkout, a `.DS_Store` sitting where a version should be. Refusing the whole listing over one
/// would make the library unusable for the ordinary reason that a person put something in it.
pub fn read_bundle(dir: &Path, name: &str, version: &str) -> std::io::Result<Option<Bundle>> {
    let manifest_path = dir.join(MANIFEST);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&manifest_path)?;
    let manifest: ManifestFile = serde_yaml::from_str(&text).unwrap_or_default();
    let files = file_hashes(dir)?;

    Ok(Some(Bundle {
        // The directory names win over the manifest's own fields, which is why they are not even
        // read: a bundle whose manifest says it is `plan@2.0` while living at `gate/1.0` is
        // addressed by where it is, because that is what a pin resolves against — and the
        // alternative is a coordinate that cannot be found.
        name: name.to_string(),
        version: version.to_string(),
        description: manifest.description,
        origin: manifest
            .origin
            .unwrap_or_else(|| format!("library:{name}@{version}")),
        owns: manifest.owns,
        hash: digest_of(&files),
        path: dir.to_string_lossy().into_owned(),
    }))
}

/// Every bundle in the library, name then version, newest version first.
///
/// A library directory that is not there is an empty library, not a failure: the ordinary state of
/// a machine that has never installed a workflow.
pub fn library(root: &Path) -> std::io::Result<Vec<Bundle>> {
    let mut found = Vec::new();
    let Ok(names) = std::fs::read_dir(root) else {
        return Ok(found);
    };
    for name_entry in names.flatten() {
        if !name_entry.file_type()?.is_dir() {
            continue;
        }
        let name = name_entry.file_name().to_string_lossy().into_owned();
        if !valid_name(&name) {
            continue;
        }
        let Ok(versions) = std::fs::read_dir(name_entry.path()) else {
            continue;
        };
        for version_entry in versions.flatten() {
            if !version_entry.file_type()?.is_dir() {
                continue;
            }
            let version = version_entry.file_name().to_string_lossy().into_owned();
            if !valid_name(&version) {
                continue;
            }
            if let Some(bundle) = read_bundle(&version_entry.path(), &name, &version)? {
                found.push(bundle);
            }
        }
    }
    found.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| compare_versions(&b.version, &a.version))
    });
    Ok(found)
}

/// Order two version strings, newest last.
///
/// Numeric segments compare as numbers, so `1.10` is above `1.9` — the mistake a plain string sort
/// makes, and one that would offer somebody a downgrade and call it an update. A named segment sorts
/// below a numbered one at the same position, which is enough to keep `1.0-rc1` under `1.0` without
/// this module pretending to implement semver.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    /// `None` is a segment that is not a number — `rc1`, `beta`, `x`.
    fn key(value: &str) -> Vec<(Option<u64>, String)> {
        value
            .split(['.', '-', '+'])
            .map(|part| (part.parse::<u64>().ok(), part.to_string()))
            .collect()
    }

    let (left, right) = (key(a), key(b));
    let zero = (Some(0), String::new());
    for index in 0..left.len().max(right.len()) {
        let l = left.get(index).unwrap_or(&zero);
        let r = right.get(index).unwrap_or(&zero);
        let ordering = match (l.0, r.0) {
            (Some(l), Some(r)) => l.cmp(&r),
            // A pre-release name sits under the release it precedes: `1.0-rc1` before `1.0`, which
            // padding with a numeric zero on the right-hand side gets exactly backwards.
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, None) => l.1.cmp(&r.1),
        };
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    std::cmp::Ordering::Equal
}

/* -------------------------------------------------------------------- pin -- */

/// What this project overrides on one node of the origin's graph.
///
/// **Rows, not a second graph.** §6.2 says the overlay is painted rather than hidden: the canvas
/// draws the origin's graph and stamps these on top, so a node this project disabled stays in the
/// picture, dotted. Fields are all optional because absent means *inherited*, which is a third
/// answer that a `bool` and a default would have collapsed into "off".
///
/// This slice does not open the graph, so it never checks that a key here names a node that exists.
/// That is the canvas's question and it is the right place for it: an overlay row for a node a
/// newer bundle removed is information — it is how somebody learns their override stopped applying
/// — and refusing to load the pin over it would take the whole workflow away instead.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeOverlay {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// One workflow this project uses, as `.ai/workflows.yaml` records it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    pub name: String,
    pub version: String,
    /// Where the bundle came from, copied off the manifest at install time. See the module header.
    pub origin: String,
    /// What the bundle hashed to when it was pinned. The whole of `drifted` rests on this field.
    pub hash: String,
    /// When this project took its own copy — and so, when it stopped receiving updates. §6.1's
    /// second named weakness is that an ejected bundle freezes in silence; this is the field that
    /// lets the page say how long the silence has lasted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ejected_at: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nodes: BTreeMap<String, NodeOverlay>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pins {
    #[serde(default)]
    pub workflows: Vec<Pin>,
}

/// A blank file, a file of nothing but comments, and a file that says `workflows: []` are one fact:
/// this project pins nothing. A parse error is a different fact and stays one.
pub fn parse_pins(contents: &str) -> Result<Pins, String> {
    let pins: Pins = match serde_yaml::from_str::<Option<Pins>>(contents) {
        Ok(Some(pins)) => pins,
        Ok(None) => Pins::default(),
        Err(error) => return Err(error.to_string()),
    };

    let mut seen = BTreeSet::new();
    for pin in &pins.workflows {
        if !valid_name(&pin.name) {
            return Err(format!("`{}` is not a usable workflow name", pin.name));
        }
        if !valid_name(&pin.version) {
            return Err(format!(
                "`{}` is not a usable version for `{}`",
                pin.version, pin.name
            ));
        }
        // The name is the identity — `install` is an upsert on it and `uninstall` a removal by it —
        // so a file naming one workflow twice is a file where neither of those means anything.
        if !seen.insert(pin.name.clone()) {
            return Err(format!("`{}` is pinned twice", pin.name));
        }
    }
    Ok(pins)
}

/// Serialise the pins back, with a line saying who writes the file.
///
/// **Re-serialising destroys comments, and here that is correct** — which is the opposite of the
/// call `OwnedFiles` makes about `.ai/autopilot.yaml`. That file is a person's, and commenting
/// `gate_command:` out is how a gate is switched off, so the app edits it as raw text. This file is
/// the app's own bookkeeping: every field in it is written by an install, an eject or an update.
/// The header says so, so that somebody who types a comment into it learns where it went.
pub fn render_pins(pins: &Pins) -> String {
    let body = serde_yaml::to_string(pins).unwrap_or_else(|_| "workflows: []\n".to_string());
    format!(
        "# Written by NucleOS. Which workflows this project uses, which version of each, and what\n\
         # this project overrides on their nodes. Values survive; comments do not — the app\n\
         # rewrites this file whole on every install, eject and update.\n{body}"
    )
}

/// The validator [`crate::ownership`] needs to let this file be written through the app.
pub fn validate_pins(contents: &str) -> Result<(), String> {
    parse_pins(contents).map(|_| ())
}

/// Read a project's pins. A project with no file pins nothing.
pub fn read_pins(project_root: &Path) -> Result<Pins, String> {
    let path = project_root.join(PINS_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_pins(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Pins::default()),
        Err(error) => Err(error.to_string()),
    }
}

/* -------------------------------------------------------------- standings -- */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    Referenced,
    Drifted,
    Ejected,
    Missing,
}

/// Which of the four, from the three facts that decide it.
///
/// **`ejected` wins over everything.** A project holding its own copy is not missing a workflow,
/// whatever the library does or does not have; the state of the origin is reported beside this
/// rather than folded into it, because "you have your own copy and the origin has moved on" is the
/// case §6.1 exists to make visible and a `missing` here would hide it.
pub fn standing(pin: &Pin, origin: Option<&Bundle>, ejected: bool) -> Standing {
    if ejected {
        return Standing::Ejected;
    }
    match origin {
        None => Standing::Missing,
        Some(bundle) if bundle.hash == pin.hash => Standing::Referenced,
        Some(_) => Standing::Drifted,
    }
}

/// One installed workflow, everything the page needs to say what it is and what to do about it.
#[derive(Debug, Clone, Serialize)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub origin: String,
    /// What was pinned. Compared against `origin_hash` by [`standing`]; served so the page can show
    /// both when they disagree, rather than asserting a difference nobody can check.
    pub hash: String,
    pub standing: Standing,
    pub ejected_at: Option<String>,
    /// What the library's copy hashes to now, or `None` when the library has nothing there.
    pub origin_hash: Option<String>,
    /// What this project's own copy hashes to, for an ejected workflow.
    pub local_hash: Option<String>,
    /// A higher version sitting in the library. An offer, never drift — see the module header.
    pub update_available: Option<String>,
    pub description: Option<String>,
    /// Files in this project the workflow declares itself the author of.
    pub owns: Vec<String>,
    /// How many nodes this project overrides, and how many of those are switched off.
    pub overridden_nodes: usize,
    pub disabled_nodes: usize,
}

/// Where an ejected copy of `name` lives in this project.
pub fn ejected_path(project_root: &Path, name: &str) -> Option<PathBuf> {
    if !valid_name(name) {
        return None;
    }
    crate::inspect::safe_join(project_root, &format!("{EJECTED_DIR}/{name}")).ok()
}

/// Every workflow this project pins, measured against the library as it is right now.
pub fn installed(project_root: &Path, library_root: &Path) -> Result<Vec<Installed>, String> {
    let pins = read_pins(project_root)?;
    let shelf = library(library_root).map_err(|error| error.to_string())?;

    let mut out = Vec::new();
    for pin in pins.workflows {
        let origin = shelf
            .iter()
            .find(|bundle| bundle.name == pin.name && bundle.version == pin.version);

        // Ejected is decided by what is on disk, not only by the field. A pin stamped `ejected_at`
        // whose folder somebody deleted is not still ejected — it is a project with no copy, and
        // saying otherwise would offer a diff against a directory that is not there.
        let local_dir = ejected_path(project_root, &pin.name).filter(|path| path.is_dir());
        let local = local_dir
            .as_ref()
            .and_then(|path| file_hashes(path).ok())
            .map(|files| digest_of(&files));
        // The manifest of the copy the project actually has, which for an ejected workflow is the
        // one that decides what it authors here. Read separately from the hash above so that a copy
        // whose `bundle.yaml` somebody deleted still counts as ejected: the folder is there, and
        // the standing is about the folder.
        let local_owns = local_dir
            .as_ref()
            .and_then(|path| read_bundle(path, &pin.name, &pin.version).ok().flatten())
            .map(|bundle| bundle.owns);

        let update = shelf
            .iter()
            .filter(|bundle| bundle.name == pin.name)
            .map(|bundle| bundle.version.clone())
            .filter(|version| {
                compare_versions(version, &pin.version) == std::cmp::Ordering::Greater
            })
            .max_by(|a, b| compare_versions(a, b));

        out.push(Installed {
            standing: standing(&pin, origin, local.is_some()),
            origin_hash: origin.map(|bundle| bundle.hash.clone()),
            local_hash: local,
            update_available: update,
            description: origin.and_then(|bundle| bundle.description.clone()),
            owns: local_owns
                .or_else(|| origin.map(|bundle| bundle.owns.clone()))
                .unwrap_or_default(),
            overridden_nodes: pin.nodes.len(),
            disabled_nodes: pin
                .nodes
                .values()
                .filter(|overlay| overlay.disabled == Some(true))
                .count(),
            name: pin.name,
            version: pin.version,
            origin: pin.origin,
            hash: pin.hash,
            ejected_at: pin.ejected_at,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/* ------------------------------------------------------------------- diff -- */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Added,
    Removed,
    Changed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileChange {
    pub path: String,
    pub change: Change,
}

/// What this project's copy has that the origin does not, and the other way round.
///
/// File-level, not line-level, and the words on the page say so. §6.1 asks for the drift to be
/// first-class information rather than silence; which files diverged is that, and a line-by-line
/// diff of five markdown files is a second problem with a second answer — the editor, one click
/// away, which §7 already made the exit for everything the app does not author.
///
/// `added` and `removed` are said from the project's side: a file the project has and the origin
/// does not is `added`, because the reader is looking at their own copy.
pub fn compare(
    mine: &BTreeMap<String, String>,
    theirs: &BTreeMap<String, String>,
) -> Vec<FileChange> {
    let mut out = Vec::new();
    for (path, hash) in mine {
        match theirs.get(path) {
            None => out.push(FileChange {
                path: path.clone(),
                change: Change::Added,
            }),
            Some(other) if other != hash => out.push(FileChange {
                path: path.clone(),
                change: Change::Changed,
            }),
            Some(_) => {}
        }
    }
    for path in theirs.keys() {
        if !mine.contains_key(path) {
            out.push(FileChange {
                path: path.clone(),
                change: Change::Removed,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/* ---------------------------------------------------------------- writing -- */

#[derive(Debug, PartialEq)]
pub enum Refused {
    /// No bundle in the library at that name and version.
    NoSuchBundle,
    /// Nothing pinned under that name in this project.
    NotInstalled,
    /// A name that cannot be a directory. See [`valid_name`].
    BadName,
    /// Already has its own copy; ejecting again would overwrite whatever is in it.
    AlreadyEjected,
    /// The bundle has no `graph.yaml`. Not a fault — skills and scripts with no sequence yet is an
    /// ordinary halfway state, and this module is not the one that decides a bundle needs a graph.
    NoGraph,
    /// It has one and it does not parse, in [`crate::workflow_graph`]'s own words.
    InvalidGraph(String),
    /// The pins file, or the ejected copy, could not be read or written.
    Io(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::NoSuchBundle => write!(f, "the library has no bundle at that version"),
            Refused::NotInstalled => write!(f, "this project does not use that workflow"),
            Refused::BadName => write!(f, "that is not a usable workflow name"),
            Refused::AlreadyEjected => {
                write!(f, "this project already has its own copy of that workflow")
            }
            Refused::NoGraph => write!(f, "this bundle has no graph in it yet"),
            Refused::InvalidGraph(detail) => write!(f, "{detail}"),
            Refused::Io(detail) => write!(f, "{detail}"),
        }
    }
}

fn save_pins(project_root: &Path, pins: &Pins) -> Result<(), Refused> {
    let path = project_root.join(PINS_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Refused::Io(error.to_string()))?;
    }
    // The same temp-then-rename `http::write_atomically` uses, for the same reason: a half-written
    // pins file is not a smaller one, it is a project whose workflows all became `missing`.
    let temp = path.with_extension("nucleos-tmp");
    std::fs::write(&temp, render_pins(pins)).map_err(|error| Refused::Io(error.to_string()))?;
    std::fs::rename(&temp, &path).map_err(|error| Refused::Io(error.to_string()))
}

/// Pin a bundle, replacing any pin of the same name.
///
/// **The overlay survives a re-pin.** Installing a different version of a workflow somebody has
/// already configured is the ordinary upgrade, and dropping their node overrides on the floor for
/// it would be a silent loss with nowhere to look it up. A node the new version does not have keeps
/// its row and stops applying, which the canvas can say out loud.
pub fn install(project_root: &Path, bundle: &Bundle) -> Result<(), Refused> {
    if !valid_name(&bundle.name) || !valid_name(&bundle.version) {
        return Err(Refused::BadName);
    }
    let mut pins = read_pins(project_root).map_err(Refused::Io)?;
    let existing = pins
        .workflows
        .iter()
        .position(|pin| pin.name == bundle.name);
    let nodes = existing
        .map(|index| pins.workflows[index].nodes.clone())
        .unwrap_or_default();

    let pin = Pin {
        name: bundle.name.clone(),
        version: bundle.version.clone(),
        origin: bundle.origin.clone(),
        hash: bundle.hash.clone(),
        // Cleared, because installing from the library is the opposite of holding your own copy.
        // The folder under `.ai/workflows/` is left alone — deleting a project's files is not
        // something an install may do quietly — and `installed` reads the disk, so it will keep
        // reporting `ejected` until somebody removes it. That is the truth of the situation.
        ejected_at: None,
        nodes,
    };
    match existing {
        Some(index) => pins.workflows[index] = pin,
        None => pins.workflows.push(pin),
    }
    pins.workflows.sort_by(|a, b| a.name.cmp(&b.name));
    save_pins(project_root, &pins)
}

/// Stop using a workflow. Removes the pin; never removes an ejected copy.
///
/// Deleting `.ai/workflows/<name>/` here would be this app deleting somebody's files as a side
/// effect of a list operation. The copy is theirs — that is what ejecting means — and it is under
/// version control where they put it.
pub fn uninstall(project_root: &Path, name: &str) -> Result<(), Refused> {
    let mut pins = read_pins(project_root).map_err(Refused::Io)?;
    let before = pins.workflows.len();
    pins.workflows.retain(|pin| pin.name != name);
    if pins.workflows.len() == before {
        return Err(Refused::NotInstalled);
    }
    save_pins(project_root, &pins)
}

/// Copy the library's bundle into the project and record when it stopped receiving updates.
///
/// Refuses when a copy is already there. Overwriting would throw away the edits that are the entire
/// reason somebody ejected — and §6.3 makes eject a deliberate choice precisely so that it is never
/// the thing that happens by accident.
pub fn eject(
    project_root: &Path,
    bundle: &Bundle,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), Refused> {
    let target = ejected_path(project_root, &bundle.name).ok_or(Refused::BadName)?;
    if target.exists() {
        return Err(Refused::AlreadyEjected);
    }
    // The pin is looked up before anything is copied. A project that does not use this workflow
    // must not end up with its files in `.ai/workflows/` and nothing recording why they are there.
    let mut pins = read_pins(project_root).map_err(Refused::Io)?;
    let Some(index) = pins
        .workflows
        .iter()
        .position(|pin| pin.name == bundle.name)
    else {
        return Err(Refused::NotInstalled);
    };

    copy_tree(Path::new(&bundle.path), &target).map_err(|error| Refused::Io(error.to_string()))?;

    pins.workflows[index].ejected_at = Some(now.to_rfc3339());
    pins.workflows[index].version = bundle.version.clone();
    pins.workflows[index].origin = bundle.origin.clone();
    pins.workflows[index].hash = bundle.hash.clone();
    save_pins(project_root, &pins)
}

/// Take the origin's current bytes: re-stamp the pin, and for an ejected workflow, re-copy.
///
/// **For an ejected workflow this replaces the project's copy**, which is what update means and why
/// the page puts the diff in front of the button. `ejected_at` restarts, because the bundle has
/// just received an update and the field's whole job is to say how long it has been since the last
/// one.
pub fn update(
    project_root: &Path,
    bundle: &Bundle,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), Refused> {
    let mut pins = read_pins(project_root).map_err(Refused::Io)?;
    let Some(index) = pins
        .workflows
        .iter()
        .position(|pin| pin.name == bundle.name)
    else {
        return Err(Refused::NotInstalled);
    };

    if pins.workflows[index].ejected_at.is_some() {
        let target = ejected_path(project_root, &bundle.name).ok_or(Refused::BadName)?;
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(|error| Refused::Io(error.to_string()))?;
        }
        copy_tree(Path::new(&bundle.path), &target)
            .map_err(|error| Refused::Io(error.to_string()))?;
        pins.workflows[index].ejected_at = Some(now.to_rfc3339());
    }

    pins.workflows[index].version = bundle.version.clone();
    pins.workflows[index].origin = bundle.origin.clone();
    pins.workflows[index].hash = bundle.hash.clone();
    save_pins(project_root, &pins)
}

/// Longest a node id may be in an overlay row.
///
/// The overlay is written before the graph is ever read — a project can override a node of a bundle
/// this machine does not have — so nothing here can check that the id names anything. A bound is
/// what stops a file from becoming a place to store something else.
pub const MAX_NODE_ID: usize = 120;

/// Set, change or clear what this project overrides on one node.
///
/// **This is the half of §6.2 that means a project does not have to eject.** Which model runs a
/// node, which tool, which command, and whether it runs here at all are the project's to decide
/// without taking a copy of anything — that is what an overlay IS. Ejecting is for changing the
/// bundle's own content, and §6.3's guard belongs there rather than here.
///
/// `None` removes the row, which is how a node goes back to inheriting. A row of all-absent fields
/// would be a third state meaning the same thing, so it is normalised away: an override that
/// overrides nothing is not an override.
pub fn set_overlay(
    project_root: &Path,
    name: &str,
    node: &str,
    overlay: Option<NodeOverlay>,
) -> Result<(), Refused> {
    let node = node.trim();
    if node.is_empty() || node.len() > MAX_NODE_ID {
        return Err(Refused::BadName);
    }
    let mut pins = read_pins(project_root).map_err(Refused::Io)?;
    let Some(pin) = pins.workflows.iter_mut().find(|pin| pin.name == name) else {
        return Err(Refused::NotInstalled);
    };

    match overlay.filter(|row| row != &NodeOverlay::default()) {
        Some(row) => {
            pin.nodes.insert(node.to_string(), row);
        }
        None => {
            pin.nodes.remove(node);
        }
    }
    save_pins(project_root, &pins)
}

/// Copy a directory tree, files and directories only.
///
/// Symlinks are skipped rather than followed, the same decision [`walk`] makes and for a stronger
/// reason here: following one would copy whatever it points at into the project's own folder, which
/// is how a bundle in the library reaches out and writes somewhere it was never given.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-08-23T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// Put a bundle in a library. Returns the library root.
    fn shelf(root: &Path, name: &str, version: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = root.join(name).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), "description: a workflow\n").unwrap();
        for (path, contents) in files {
            let target = dir.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        root.to_path_buf()
    }

    fn bundle_at(root: &Path, name: &str, version: &str) -> Bundle {
        read_bundle(&root.join(name).join(version), name, version)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_bundles_hash_is_over_its_bytes_and_nothing_about_a_graph() {
        let temp = tempfile::tempdir().unwrap();
        let root = shelf(
            temp.path(),
            "harness",
            "1.0",
            &[("graph.yaml", "nodes: []"), ("skills/plan.md", "plan")],
        );
        let before = bundle_at(&root, "harness", "1.0").hash;

        // A file this module has never heard of changes the hash. That is the property that lets
        // the serialisation format stay undecided until the canvas needs it.
        std::fs::write(root.join("harness/1.0/skills/plan.md"), "plan, differently").unwrap();
        let after = bundle_at(&root, "harness", "1.0").hash;

        assert_ne!(before, after);
        assert!(before.starts_with("sha256:"));
    }

    #[test]
    fn two_files_whose_names_run_together_do_not_hash_alike() {
        let mut left = BTreeMap::new();
        left.insert("ab".to_string(), "x".to_string());
        left.insert("c".to_string(), "y".to_string());
        let mut right = BTreeMap::new();
        right.insert("a".to_string(), "x".to_string());
        right.insert("bc".to_string(), "y".to_string());
        assert_ne!(digest_of(&left), digest_of(&right));
    }

    #[test]
    fn a_directory_in_the_library_with_no_manifest_is_skipped_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("scratch/notes")).unwrap();
        let root = shelf(temp.path(), "harness", "1.0", &[]);
        let found = library(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "harness");
    }

    #[test]
    fn ten_is_a_later_version_than_nine() {
        assert_eq!(compare_versions("1.10", "1.9"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("1.0", "1.0.0"), std::cmp::Ordering::Equal);
        // A release beats its own candidate, which padding with a numeric zero gets backwards.
        assert_eq!(compare_versions("1.0-rc1", "1.0"), std::cmp::Ordering::Less);
        assert_eq!(compare_versions("2.0", "10.0"), std::cmp::Ordering::Less);
    }

    #[test]
    fn a_file_of_nothing_but_comments_pins_nothing_rather_than_failing() {
        assert_eq!(parse_pins("# nothing here yet\n").unwrap(), Pins::default());
        assert_eq!(parse_pins("").unwrap(), Pins::default());
        assert_eq!(parse_pins("workflows: []\n").unwrap(), Pins::default());
    }

    #[test]
    fn one_workflow_pinned_twice_is_a_file_that_means_nothing() {
        let text = "workflows:\n  - {name: a, version: '1', origin: x, hash: h}\n  - {name: a, version: '2', origin: x, hash: h}\n";
        assert!(parse_pins(text).unwrap_err().contains("pinned twice"));
    }

    #[test]
    fn a_pin_naming_a_path_is_refused_before_it_can_be_joined_onto_one() {
        let text = "workflows:\n  - {name: ../../etc, version: '1', origin: x, hash: h}\n";
        assert!(parse_pins(text).unwrap_err().contains("not a usable"));
        assert!(!valid_name("../etc"));
        assert!(!valid_name(".."));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("a/b"));
        assert!(valid_name("nucleos-default"));
        assert!(valid_name("1.0.0"));
    }

    #[test]
    fn the_overlay_round_trips_through_the_file_the_app_rewrites() {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            "review".to_string(),
            NodeOverlay {
                disabled: Some(true),
                ..NodeOverlay::default()
            },
        );
        let pins = Pins {
            workflows: vec![Pin {
                name: "harness".into(),
                version: "1.0".into(),
                origin: "library:harness@1.0".into(),
                hash: "sha256:abc".into(),
                ejected_at: None,
                nodes,
            }],
        };
        let text = render_pins(&pins);
        assert!(text.starts_with("# Written by NucleOS."));
        assert_eq!(parse_pins(&text).unwrap(), pins);
    }

    #[test]
    fn installing_records_where_the_bundle_came_from_and_what_it_hashed_to() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let root = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );
        let bundle = bundle_at(&root, "harness", "1.0");

        install(&project, &bundle).unwrap();
        let pins = read_pins(&project).unwrap();
        assert_eq!(pins.workflows.len(), 1);
        assert_eq!(pins.workflows[0].origin, "library:harness@1.0");
        assert_eq!(pins.workflows[0].hash, bundle.hash);
    }

    #[test]
    fn a_bundle_edited_under_its_own_pin_is_drifted_and_not_referenced() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let root = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );
        install(&project, &bundle_at(&root, "harness", "1.0")).unwrap();

        assert_eq!(
            installed(&project, &root).unwrap()[0].standing,
            Standing::Referenced
        );

        std::fs::write(root.join("harness/1.0/g.yaml"), "b").unwrap();
        let after = installed(&project, &root).unwrap();
        assert_eq!(after[0].standing, Standing::Drifted);
        // Both hashes are served, so the page states a difference somebody can check rather than
        // asserting one.
        assert_ne!(after[0].hash, after[0].origin_hash.clone().unwrap());
    }

    #[test]
    fn a_newer_version_beside_the_pinned_one_is_an_offer_and_not_drift() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = temp.path().join("lib");
        shelf(&lib, "harness", "1.0", &[("g.yaml", "a")]);
        install(&project, &bundle_at(&lib, "harness", "1.0")).unwrap();
        shelf(&lib, "harness", "1.1", &[("g.yaml", "b")]);

        let rows = installed(&project, &lib).unwrap();
        assert_eq!(rows[0].standing, Standing::Referenced);
        assert_eq!(rows[0].update_available.as_deref(), Some("1.1"));
    }

    #[test]
    fn a_pin_the_library_cannot_answer_is_missing_and_still_says_where_it_came_from() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );
        install(&project, &bundle_at(&lib, "harness", "1.0")).unwrap();

        // The new machine: the pin travelled with the repository, the bundle did not.
        let empty = temp.path().join("other-machine");
        std::fs::create_dir_all(&empty).unwrap();
        let rows = installed(&project, &empty).unwrap();
        assert_eq!(rows[0].standing, Standing::Missing);
        assert_eq!(rows[0].origin, "library:harness@1.0");
        assert!(rows[0].origin_hash.is_none());
    }

    #[test]
    fn ejecting_copies_the_bundle_and_records_when_the_updates_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("skills/plan.md", "plan")],
        );
        let bundle = bundle_at(&lib, "harness", "1.0");
        install(&project, &bundle).unwrap();
        eject(&project, &bundle, now()).unwrap();

        assert!(
            project
                .join(".ai/workflows/harness/skills/plan.md")
                .is_file()
        );
        let rows = installed(&project, &lib).unwrap();
        assert_eq!(rows[0].standing, Standing::Ejected);
        assert_eq!(
            rows[0].ejected_at.as_deref(),
            Some("2026-08-23T10:00:00+00:00")
        );
    }

    #[test]
    fn ejecting_twice_refuses_rather_than_overwriting_the_edits_that_were_the_point() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );
        let bundle = bundle_at(&lib, "harness", "1.0");
        install(&project, &bundle).unwrap();
        eject(&project, &bundle, now()).unwrap();

        std::fs::write(project.join(".ai/workflows/harness/g.yaml"), "mine").unwrap();
        assert_eq!(
            eject(&project, &bundle, now()),
            Err(Refused::AlreadyEjected)
        );
        assert_eq!(
            std::fs::read_to_string(project.join(".ai/workflows/harness/g.yaml")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn an_ejected_copy_whose_folder_was_deleted_is_not_still_ejected() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );
        let bundle = bundle_at(&lib, "harness", "1.0");
        install(&project, &bundle).unwrap();
        eject(&project, &bundle, now()).unwrap();
        std::fs::remove_dir_all(project.join(".ai/workflows/harness")).unwrap();

        // The field still says it was ejected; the disk decides, and the disk says there is no copy.
        assert_eq!(
            installed(&project, &lib).unwrap()[0].standing,
            Standing::Referenced
        );
    }

    #[test]
    fn the_diff_names_the_files_that_diverged_from_the_projects_side() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("a.md", "one"), ("b.md", "two")],
        );
        let bundle = bundle_at(&lib, "harness", "1.0");
        install(&project, &bundle).unwrap();
        eject(&project, &bundle, now()).unwrap();

        let mine_dir = project.join(".ai/workflows/harness");
        std::fs::write(mine_dir.join("a.md"), "one, changed").unwrap();
        std::fs::write(mine_dir.join("c.md"), "new").unwrap();
        std::fs::remove_file(mine_dir.join("b.md")).unwrap();

        let changes = compare(
            &file_hashes(&mine_dir).unwrap(),
            &file_hashes(Path::new(&bundle.path)).unwrap(),
        );
        assert_eq!(
            changes,
            vec![
                FileChange {
                    path: "a.md".into(),
                    change: Change::Changed
                },
                FileChange {
                    path: "b.md".into(),
                    change: Change::Removed
                },
                FileChange {
                    path: "c.md".into(),
                    change: Change::Added
                },
            ]
        );
    }

    #[test]
    fn reinstalling_a_different_version_keeps_the_nodes_this_project_overrode() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = temp.path().join("lib");
        shelf(&lib, "harness", "1.0", &[("g.yaml", "a")]);
        shelf(&lib, "harness", "1.1", &[("g.yaml", "b")]);
        install(&project, &bundle_at(&lib, "harness", "1.0")).unwrap();

        let mut pins = read_pins(&project).unwrap();
        pins.workflows[0].nodes.insert(
            "review".into(),
            NodeOverlay {
                disabled: Some(true),
                ..NodeOverlay::default()
            },
        );
        save_pins(&project, &pins).unwrap();

        install(&project, &bundle_at(&lib, "harness", "1.1")).unwrap();
        let after = read_pins(&project).unwrap();
        assert_eq!(after.workflows.len(), 1);
        assert_eq!(after.workflows[0].version, "1.1");
        assert_eq!(after.workflows[0].nodes["review"].disabled, Some(true));
    }

    #[test]
    fn updating_an_ejected_workflow_replaces_the_copy_and_restarts_the_clock() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = temp.path().join("lib");
        shelf(&lib, "harness", "1.0", &[("g.yaml", "a")]);
        let old = bundle_at(&lib, "harness", "1.0");
        install(&project, &old).unwrap();
        eject(&project, &old, now()).unwrap();
        std::fs::write(project.join(".ai/workflows/harness/g.yaml"), "mine").unwrap();

        shelf(&lib, "harness", "1.1", &[("g.yaml", "b")]);
        let later = now() + chrono::Duration::days(30);
        update(&project, &bundle_at(&lib, "harness", "1.1"), later).unwrap();

        assert_eq!(
            std::fs::read_to_string(project.join(".ai/workflows/harness/g.yaml")).unwrap(),
            "b"
        );
        let rows = installed(&project, &lib).unwrap();
        assert_eq!(rows[0].version, "1.1");
        assert_eq!(rows[0].standing, Standing::Ejected);
        assert_eq!(
            rows[0].ejected_at.as_deref(),
            Some("2026-09-22T10:00:00+00:00")
        );
    }

    /// The overlay is how a project changes a node WITHOUT taking a copy of the bundle, which is
    /// the whole reason it exists. Clearing a row is how the node goes back to inheriting.
    #[test]
    fn overriding_a_node_needs_no_copy_of_anything_and_clears_back_to_inherited() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );
        install(&project, &bundle_at(&lib, "harness", "1.0")).unwrap();

        set_overlay(
            &project,
            "harness",
            "plan",
            Some(NodeOverlay {
                model: Some("haiku".into()),
                ..NodeOverlay::default()
            }),
        )
        .unwrap();
        let rows = installed(&project, &lib).unwrap();
        assert_eq!(rows[0].overridden_nodes, 1);
        // Still referenced: overriding is not ejecting, and a project that had to take a copy to
        // change a model would take one every time.
        assert_eq!(rows[0].standing, Standing::Referenced);

        set_overlay(&project, "harness", "plan", None).unwrap();
        assert_eq!(installed(&project, &lib).unwrap()[0].overridden_nodes, 0);
    }

    /// An override that overrides nothing is not an override, and does not become a row that would
    /// stamp the project seal on a node nobody touched.
    #[test]
    fn an_override_of_nothing_does_not_become_a_row() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );
        install(&project, &bundle_at(&lib, "harness", "1.0")).unwrap();

        set_overlay(&project, "harness", "plan", Some(NodeOverlay::default())).unwrap();
        assert!(read_pins(&project).unwrap().workflows[0].nodes.is_empty());
        assert_eq!(
            set_overlay(&project, "harness", "  ", None),
            Err(Refused::BadName)
        );
    }

    #[test]
    fn uninstalling_removes_the_pin_and_never_the_projects_own_copy() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );
        let bundle = bundle_at(&lib, "harness", "1.0");
        install(&project, &bundle).unwrap();
        eject(&project, &bundle, now()).unwrap();

        uninstall(&project, "harness").unwrap();
        assert!(read_pins(&project).unwrap().workflows.is_empty());
        assert!(project.join(".ai/workflows/harness/g.yaml").is_file());
        assert_eq!(uninstall(&project, "harness"), Err(Refused::NotInstalled));
    }

    #[test]
    fn ejecting_something_this_project_does_not_use_leaves_no_files_behind() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let lib = shelf(
            &temp.path().join("lib"),
            "harness",
            "1.0",
            &[("g.yaml", "a")],
        );

        assert_eq!(
            eject(&project, &bundle_at(&lib, "harness", "1.0"), now()),
            Err(Refused::NotInstalled)
        );
        assert!(!project.join(".ai/workflows/harness").exists());
    }
}
