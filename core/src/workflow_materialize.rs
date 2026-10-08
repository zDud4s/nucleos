//! §spec motor-de-workflows
//!
//! Put a pinned bundle's files where agents read them, and never over a file somebody edited.
//!
//! [`crate::workflows`] keeps the library, the pins and the drift between them, and until this
//! module nothing ever put a bundle's files into a project. A workflow was pinned and then read by
//! nobody: agents read `.ai/workflow/`, `.claude/skills/` and the like in the checkout they work
//! in, and those were placed there by hand — in this repository by a gitignored script that copied
//! the main checkout's core over every worktree, overwrote blindly, and on 2026-09-19 took a
//! worktree's own edits with it, with no history to bring them back from.
//!
//! # What a bundle provides is its `owns:` list
//!
//! The manifest already declared the project files a workflow authors, for the ownership fence
//! (`ownership.rs`). That list is what is materialized, and nothing else in the bundle: the graph,
//! the manifest and anything else are the library's to keep. An entry is a relative path in the
//! spelling [`crate::ownership::normalise`] accepts — no `..`, nothing absolute, no backslash — and
//! it names either a FILE in the bundle or a DIRECTORY of it, in which case every file under it is
//! provided. A trailing slash is accepted and changes nothing. The bundle directory mirrors the
//! project: the bundle's `.ai/workflow/workflow.md` is the project's `.ai/workflow/workflow.md`.
//!
//! This module still parses nothing but the manifest. Which files a workflow consists of is the
//! bundle's to say; how they are copied is decided by their hashes alone.
//!
//! # The record is what makes overwriting safe
//!
//! Every file this module writes is recorded with the hash of what it wrote, in
//! `~/.nucleos/projects/<id>/materialized.yaml` for the project's main checkout and in one file per
//! worktree under `materialized/` beside it. The record is keyed by bundle name and carries the
//! version it was written from. On every later run, each file the bundle provides is one of:
//!
//! - **missing** — written;
//! - **identical to the bundle's** — left alone, and recorded (adopted) if it was not yet;
//! - **unchanged since this module wrote it** — its hash is the recorded one — overwritten;
//! - **edited** — present, recorded, and not what was recorded — NEVER overwritten, reported as a
//!   conflict with both hashes and the path of the bundle's copy, which is enough to show a diff;
//! - **there before and never recorded** — a conflict too, since nothing says whose it is.
//!
//! A file the previous version provided and the new one does not is deleted when it is unchanged
//! since it was written, and kept and reported when it was edited.
//!
//! **Refuse rather than guess.** A record that does not parse stops the whole run: without it an
//! unchanged file and an edited one look the same, and the only safe reading of *I cannot tell*
//! is not to write.
//!
//! # Files the daemon installs are never a bundle's
//!
//! `.claude/settings.json` and `.claude/hooks/` carry the classifier hook that onboarding wires
//! (`autopilot::wire_classifier_hook`). A bundle that provided them would unwire the gate every
//! tool call passes through, so those paths are skipped whatever a manifest says, and reported.
//!
//! # The managed block in `AGENTS.md`
//!
//! `AGENTS.md` is the project's file — its own notes, gotchas and invariants — but its top carries
//! the workflow's entry point between two marker lines, and that region used to be kept by hand.
//! A manifest may name one bundle file as `agents_block:`; that file is the whole region, verbatim,
//! its first line the begin marker and its last the end marker — the installer's own
//! `agents-block.md` has always been written that way. Only the region is ever written:
//! replaced where the markers are, prepended with one blank line where they are not, and the whole
//! file created where there is none. Everything outside the markers is never touched, and the
//! file's own line endings are kept. The bundle never owns `AGENTS.md` as a whole file, so a
//! bundle naming a block and also providing `AGENTS.md` has the latter skipped as reserved.
//!
//! The region follows the per-file rules above, recorded under the key [`AGENTS_BLOCK_KEY`] with
//! the hash of the region as written, line breaks counted as `\n` so a checkout that converts them
//! does not read as an edit. An edited region is a conflict and stays; one there before and never
//! recorded is adopted only when it is already the bundle's; a version that stops naming a block
//! takes the region out when it is unchanged, and leaves it recorded and reported when it is not.
//! A block source that is missing, not text or not framed by the markers stops the run before
//! anything is written: without it, "the new version dropped the block" and "the block could not be
//! read" look the same, and a source without markers would write a region no later run could find.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::workflows;

/// Paths a bundle may never provide, because the daemon installs them itself. See the module
/// header. A path equal to one of these, or under one ending in `/`, is skipped.
const RESERVED: &[&str] = &[
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".claude/hooks/",
];

/// Whether `rel` is a path the daemon installs and no bundle may write.
pub fn reserved(rel: &str) -> bool {
    // Derived from the hook's own path rather than written out a second time, so moving the hook
    // cannot leave a bundle able to overwrite it.
    let hook_dir = crate::autopilot::HOOK_SCRIPT
        .rsplit_once('/')
        .map_or("", |(dir, _)| dir);
    RESERVED.iter().any(|entry| match entry.strip_suffix('/') {
        Some(dir) => rel == dir || rel.starts_with(&format!("{dir}/")),
        None => rel == *entry,
    }) || (!hook_dir.is_empty() && rel.starts_with(&format!("{hook_dir}/")))
}

/// The record key of the managed block in `AGENTS.md`. Not a path: `#` keeps it from ever being
/// mistaken for a file the bundle provides, and the removal pass skips it for the same reason.
pub const AGENTS_BLOCK_KEY: &str = "AGENTS.md#managed-block";

/* ----------------------------------------------------------------- record -- */

/// What was written into one checkout, per bundle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub bundles: BTreeMap<String, RecordedBundle>,
}

/// One bundle's files as they were last written: relative path to the hash of the bytes written.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordedBundle {
    pub version: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

/// Read a record. An absent file is an empty record — nothing was ever written — and anything
/// that does not parse is an error the caller must stop on (see the module header).
pub fn read_record(path: &Path) -> Result<Record, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_yaml::from_str::<Option<Record>>(&text) {
            Ok(record) => Ok(record.unwrap_or_default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn render_record(record: &Record) -> String {
    let body = serde_yaml::to_string(record).unwrap_or_else(|_| "bundles: {}\n".to_string());
    format!(
        "# Written by NucleOS. What each workflow bundle last wrote into this checkout, and the\n\
         # hash of each file as written: a file whose hash still matches may be replaced by the next\n\
         # version, and one that does not was edited and never is. Rewritten whole on every run.\n{body}"
    )
}

/// The record for one worktree of a project, under the project's state directory.
///
/// Named after the worktree's directory, for a person looking at the folder, and a hash of its
/// full path, because two hand-made worktrees called `feature` in two different parents are two
/// checkouts and must not share one record.
///
/// The path is hashed in one spelling whoever names it: the daemon names a tree it opened by the
/// path it built, and `POST /workflows/sync` by a canonical one, which on Windows carries a `\\?\`
/// prefix and may differ in case. Hashed as given, one tree would get two records, and a file
/// edited in it before a hand-run sync would read as untracked rather than edited.
pub fn worktree_record(state_dir: &Path, worktree: &Path) -> PathBuf {
    let spelled = worktree.to_string_lossy().replace('\\', "/");
    let spelled = match spelled.strip_prefix("//?/") {
        Some(rest) => match rest.strip_prefix("UNC/") {
            Some(share) => format!("//{share}"),
            None => rest.to_string(),
        },
        None => spelled,
    };
    let full = if cfg!(windows) {
        spelled.to_lowercase()
    } else {
        spelled
    };
    let tag = &workflows::hash_of(full.as_bytes())[..8];
    let name: String = worktree
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    let name = name.trim_start_matches('.');
    let file = if name.is_empty() {
        format!("{tag}.yaml")
    } else {
        format!("{name}-{tag}.yaml")
    };
    state_dir
        .join(crate::project_state::WORKTREE_RECORDS_DIR)
        .join(file)
}

/* ------------------------------------------------------------------ owned -- */

/// The entries of an `owns:` list in the one spelling this module compares, dropping any that
/// [`crate::ownership::normalise`] refuses — which is how `..`, an absolute path or a backslash
/// never reaches a join.
pub fn owns_entries(owns: &[String]) -> Vec<String> {
    owns.iter()
        .filter_map(|entry| crate::ownership::normalise(entry))
        .collect()
}

/// Whether `file` (a bundle-relative path) is provided by `entries`: named exactly, or under a
/// directory one of them names.
pub fn provided(entries: &[String], file: &str) -> bool {
    entries
        .iter()
        .any(|entry| file == entry || file.starts_with(&format!("{entry}/")))
}

/* ------------------------------------------------------------------ report -- */

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// Written by this module, then changed in the checkout. Never overwritten.
    Edited,
    /// There before this module ever wrote it, and not the bundle's bytes.
    Untracked,
    /// The new version no longer provides it, and it was edited, so it is kept.
    RemovedEdited,
    /// The path resolves outside the checkout (a link or a junction on the way). Not touched.
    Unsafe,
}

/// A file left alone, with enough to show a diff.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conflict {
    pub path: String,
    pub kind: ConflictKind,
    /// What the checkout holds now. `None` when there is no file to hash — a directory in its
    /// place, or a path that could not be resolved.
    pub local_hash: Option<String>,
    /// What this module last wrote there, when it ever did.
    pub recorded_hash: Option<String>,
    /// What the bundle provides now, when it still provides it.
    pub bundle_hash: Option<String>,
    /// The bundle's copy of the file, absolute, for the diff. `None` for a removed file.
    pub bundle_file: Option<String>,
}

/// Whether a run writes or only says what it would write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Apply,
    Preview,
}

/// What one bundle's materialization did — or, in preview, would do.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Report {
    pub name: String,
    pub version: String,
    /// Whether anything was written. `false` for a preview, whose lists say what WOULD happen.
    pub applied: bool,
    /// Missing, and written.
    pub written: Vec<String>,
    /// Unchanged since the last write, and replaced by the new version.
    pub updated: Vec<String>,
    /// Already the bundle's bytes, and recorded as this module's from now on.
    pub adopted: Vec<String>,
    /// No longer provided, unchanged since written, and removed.
    pub deleted: Vec<String>,
    /// Already current. Counted, not listed: it is the ordinary case.
    pub unchanged: usize,
    pub conflicts: Vec<Conflict>,
    /// Paths the manifest named that the daemon installs itself. See [`reserved`].
    pub reserved: Vec<String>,
}

impl Report {
    /// Whether the bundle provided nothing here at all — an `owns:` list that is empty, or that
    /// names nothing the bundle has. Not worth a word in a feed line.
    pub fn is_empty(&self) -> bool {
        self.written.is_empty()
            && self.updated.is_empty()
            && self.adopted.is_empty()
            && self.deleted.is_empty()
            && self.unchanged == 0
            && self.conflicts.is_empty()
            && self.reserved.is_empty()
    }

    /// One line for the feed.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (count, word) in [
            (self.written.len(), "written"),
            (self.updated.len(), "updated"),
            (self.adopted.len(), "adopted"),
            (self.deleted.len(), "removed"),
        ] {
            if count > 0 {
                parts.push(format!("{count} {word}"));
            }
        }
        if parts.is_empty() {
            parts.push("nothing to change".to_string());
        }
        if !self.conflicts.is_empty() {
            parts.push(format!(
                "{} left alone because they were edited here",
                self.conflicts.len()
            ));
        }
        format!("{}@{}: {}", self.name, self.version, parts.join(", "))
    }
}

/* ---------------------------------------------------------- managed block -- */

/// The project file the managed block lives in.
const AGENTS_FILE: &str = "AGENTS.md";
const BLOCK_BEGIN: &str = "# >>> AI WORKFLOW MANAGED BLOCK >>>";
const BLOCK_END: &str = "# <<< AI WORKFLOW MANAGED BLOCK <<<";

/// The block a bundle offers: rendered with `\n` line breaks, its hash, and the bundle file it
/// came from (for a conflict's diff).
struct OfferedBlock {
    rendered: String,
    hash: String,
    source_file: String,
}

/// The region a source is: the source itself, with `\n` line breaks and its last line ended.
/// `None` unless its first line is the begin marker, its last the end marker, and neither marker
/// appears anywhere else — a marker inside would make the written region end where the next run
/// cannot tell it does. Packaging refuses such a source with the same test (`workflow_package`).
pub fn render_block(source: &str) -> Option<String> {
    let text = source.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let body = text.trim_end_matches('\n');
    let lines: Vec<&str> = body.split('\n').collect();
    let framed = lines.len() >= 2
        && lines.first() == Some(&BLOCK_BEGIN)
        && lines.last() == Some(&BLOCK_END)
        && lines[1..lines.len() - 1]
            .iter()
            .all(|line| *line != BLOCK_BEGIN && *line != BLOCK_END);
    framed.then(|| format!("{body}\n"))
}

/// Read the block `source` (a bundle-relative path from the manifest) names. Any failure is the
/// whole run's: see the module header.
fn read_block(bundle_dir: &Path, source: &str) -> Result<OfferedBlock, String> {
    let rel = crate::ownership::normalise(source).ok_or_else(|| {
        format!("agents_block `{source}` is not a relative path inside the bundle")
    })?;
    let file = bundle_dir.join(&rel);
    // Not followed through a link, for the reason `workflows::file_hashes` skips them.
    let is_file = std::fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file());
    if !is_file {
        return Err(format!("agents_block `{rel}` is not a file in the bundle"));
    }
    let bytes = std::fs::read(&file).map_err(|error| format!("agents_block `{rel}`: {error}"))?;
    let text =
        String::from_utf8(bytes).map_err(|_| format!("agents_block `{rel}` is not UTF-8 text"))?;
    let rendered = render_block(&text).ok_or_else(|| {
        format!(
            "agents_block `{rel}` must start with `{BLOCK_BEGIN}` and end with `{BLOCK_END}`, \
             with neither marker in between"
        )
    })?;
    Ok(OfferedBlock {
        hash: workflows::hash_of(rendered.as_bytes()),
        rendered,
        source_file: file.to_string_lossy().into_owned(),
    })
}

/// Where the markers are in a file's text.
enum Region {
    Absent,
    /// From the start of the begin marker's line to the end of the end marker's, line break
    /// included.
    At(std::ops::Range<usize>),
    /// A begin marker and no end marker after it: nothing says where the block stops, so nothing
    /// in it is touched.
    Unclosed,
}

fn find_region(text: &str) -> Region {
    let mut offset = 0;
    let mut begin = None;
    for line in text.split_inclusive('\n') {
        let bom = if line.starts_with('\u{feff}') { 3 } else { 0 };
        let bare = line[bom..].trim_end_matches('\n').trim_end_matches('\r');
        match begin {
            None if bare == BLOCK_BEGIN => begin = Some(offset + bom),
            Some(start) if bare == BLOCK_END => return Region::At(start..offset + line.len()),
            _ => {}
        }
        offset += line.len();
    }
    if begin.is_some() {
        Region::Unclosed
    } else {
        Region::Absent
    }
}

/// The region's hash, line breaks counted as `\n` and the last line always ended, so it compares
/// with a rendered block whatever the checkout did to its line endings.
fn region_hash(region: &str) -> String {
    let mut text = region.replace("\r\n", "\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    workflows::hash_of(text.as_bytes())
}

/// `text` with the region at `range` taken out, or `None` when nothing would be left.
///
/// A block at the top was prepended with one blank line after it, and that line goes with it. A
/// file that held nothing but the block was this module's to create, and goes whole.
fn without_region(text: &str, range: std::ops::Range<usize>, eol: &str) -> Option<String> {
    let head = &text[..range.start];
    let mut rest = &text[range.end..];
    if head.trim_start_matches('\u{feff}').is_empty() {
        rest = rest.strip_prefix(eol).unwrap_or(rest);
    }
    let left = format!("{head}{rest}");
    (!left.trim_start_matches('\u{feff}').is_empty()).then_some(left)
}

/// What one run does to `AGENTS.md`, decided before anything is written.
struct BlockPlan {
    target: PathBuf,
    /// The whole new file — `Some(None)` to delete it, when the block was all it held — and what
    /// the record holds once that is on disk.
    change: Option<(Option<String>, Option<String>)>,
    /// What the record holds when nothing changes, or when the change fails to land.
    keep: Option<String>,
}

/// Decide the managed block's fate, reporting it. `None` when there is nothing to decide or the
/// file cannot be safely reached.
fn plan_block(
    checkout: &Path,
    offered: Option<&OfferedBlock>,
    recorded: Option<&str>,
    report: &mut Report,
) -> Option<BlockPlan> {
    if offered.is_none() && recorded.is_none() {
        return None;
    }
    let key = AGENTS_BLOCK_KEY.to_string();
    let keep = recorded.map(str::to_string);
    let conflict = |kind, local: Option<String>| Conflict {
        path: key.clone(),
        kind,
        local_hash: local,
        recorded_hash: keep.clone(),
        bundle_hash: offered.map(|block| block.hash.clone()),
        bundle_file: offered.map(|block| block.source_file.clone()),
    };
    // What an edit is called: still offered, it is an edit; no longer offered, a kept removal.
    let edited = if offered.is_some() {
        ConflictKind::Edited
    } else {
        ConflictKind::RemovedEdited
    };

    let Ok(target) = crate::inspect::safe_write_target(checkout, AGENTS_FILE) else {
        if offered.is_some() {
            report.conflicts.push(conflict(ConflictKind::Unsafe, None));
        }
        // Not dropped from the record: an unreachable file is not evidence the block is gone.
        return keep.map(|hash| BlockPlan {
            target: checkout.join(AGENTS_FILE),
            change: None,
            keep: Some(hash),
        });
    };
    let text = match std::fs::symlink_metadata(&target) {
        Err(_) => None,
        Ok(meta) if meta.is_file() => {
            match std::fs::read(&target)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
            {
                Some(text) => Some(text),
                None => {
                    // Not text: no marker can be found in it, and writing back would corrupt it.
                    let kind = keep.as_ref().map_or(ConflictKind::Untracked, |_| edited);
                    report.conflicts.push(conflict(kind, None));
                    return Some(BlockPlan {
                        target,
                        change: None,
                        keep,
                    });
                }
            }
        }
        Ok(_) => {
            if offered.is_some() {
                report
                    .conflicts
                    .push(conflict(ConflictKind::Untracked, None));
            }
            return Some(BlockPlan {
                target,
                change: None,
                keep,
            });
        }
    };

    let eol = match &text {
        Some(text) if text.contains("\r\n") => "\r\n",
        _ => "\n",
    };
    let region = text.as_deref().map_or(Region::Absent, find_region);
    let mut plan = BlockPlan {
        target,
        change: None,
        keep: keep.clone(),
    };

    match offered {
        Some(block) => {
            let rendered = block.rendered.replace('\n', eol);
            let written = Some(block.hash.clone());
            match (&text, region) {
                (None, _) => {
                    report.written.push(key.clone());
                    plan.change = Some((Some(rendered), written));
                }
                (Some(text), Region::Absent) => {
                    report.written.push(key.clone());
                    let (bom, body) = text
                        .strip_prefix('\u{feff}')
                        .map_or(("", text.as_str()), |body| ("\u{feff}", body));
                    plan.change = Some((Some(format!("{bom}{rendered}{eol}{body}")), written));
                }
                (Some(_), Region::Unclosed) => {
                    let kind = keep.as_ref().map_or(ConflictKind::Untracked, |_| edited);
                    report.conflicts.push(conflict(kind, None));
                }
                (Some(text), Region::At(range)) => {
                    let local = region_hash(&text[range.clone()]);
                    if local == block.hash {
                        if recorded == Some(block.hash.as_str()) {
                            report.unchanged += 1;
                        } else {
                            report.adopted.push(key.clone());
                        }
                        plan.keep = written;
                    } else if recorded == Some(local.as_str()) {
                        report.updated.push(key.clone());
                        let replaced =
                            format!("{}{rendered}{}", &text[..range.start], &text[range.end..]);
                        plan.change = Some((Some(replaced), written));
                    } else if recorded.is_some() {
                        // Kept with what was written, so the next run still sees an edit.
                        report.conflicts.push(conflict(edited, Some(local)));
                    } else {
                        report
                            .conflicts
                            .push(conflict(ConflictKind::Untracked, Some(local)));
                    }
                }
            }
        }
        None => match (&text, region) {
            // Gone already: nothing to remove, and nothing left to remember.
            (None, _) | (Some(_), Region::Absent) => plan.keep = None,
            (Some(_), Region::Unclosed) => report.conflicts.push(conflict(edited, None)),
            (Some(text), Region::At(range)) => {
                let local = region_hash(&text[range.clone()]);
                if recorded == Some(local.as_str()) {
                    report.deleted.push(key.clone());
                    plan.change = Some((without_region(text, range, eol), None));
                } else {
                    report.conflicts.push(conflict(edited, Some(local)));
                }
            }
        },
    }
    Some(plan)
}

/* ------------------------------------------------------------ materialize -- */

fn io(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// What the checkout holds at `target`: `Ok(None)` for nothing, `Err(())` for something that is not
/// a file, `Ok(Some(hash))` for a file.
fn local_hash(target: &Path) -> Result<Option<String>, ()> {
    match std::fs::symlink_metadata(target) {
        Err(_) => Ok(None),
        Ok(meta) if meta.is_file() => std::fs::read(target)
            .map(|bytes| Some(workflows::hash_of(&bytes)))
            .map_err(|_| ()),
        Ok(_) => Err(()),
    }
}

/// Remove directories left empty by a deletion, up to (never including) `root`.
fn prune_empty_parents(root: &Path, file: &Path) {
    let mut dir = file.parent();
    while let Some(current) = dir {
        if current == root || !current.starts_with(root) {
            return;
        }
        // `remove_dir` refuses a directory that is not empty, which is the whole test.
        if std::fs::remove_dir(current).is_err() {
            return;
        }
        dir = current.parent();
    }
}

/// Materialize one bundle into `checkout`, recording what was written in `record_path`.
///
/// `bundle` is read from its own directory (`bundle.path`): the library's version directory, or a
/// project's ejected copy. Returns the report, or an error when nothing could be decided safely —
/// an unreadable record, or a bundle that cannot be walked. A per-file failure to write is an
/// error too, and the record is saved first for every file already written, so a second run
/// resumes rather than reporting its own writes as somebody's edits.
pub fn materialize(
    bundle: &workflows::Bundle,
    checkout: &Path,
    record_path: &Path,
    mode: Mode,
) -> Result<Report, String> {
    let bundle_dir = Path::new(&bundle.path);
    let mut record = read_record(record_path)?;
    let before = record.clone();
    let previous = record
        .bundles
        .get(&bundle.name)
        .map(|recorded| recorded.files.clone())
        .unwrap_or_default();

    let entries = owns_entries(&bundle.owns);
    let mut report = Report {
        name: bundle.name.clone(),
        version: bundle.version.clone(),
        applied: mode == Mode::Apply,
        ..Report::default()
    };

    // Read before anything is written: a block that cannot be read stops the run (module header).
    let block = match &bundle.agents_block {
        Some(source) => Some(read_block(bundle_dir, source)?),
        None => None,
    };

    let mut offered = BTreeMap::new();
    if !entries.is_empty() {
        for (path, hash) in workflows::file_hashes(bundle_dir).map_err(io)? {
            if !provided(&entries, &path) || path == AGENTS_BLOCK_KEY {
                continue;
            }
            // With a block, `AGENTS.md` is the project's file with one region of ours in it, and
            // a whole-file copy would overwrite the project's part.
            if reserved(&path) || (block.is_some() && path == AGENTS_FILE) {
                report.reserved.push(path);
                continue;
            }
            offered.insert(path, hash);
        }
    }

    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut outcome: Result<(), String> = Ok(());

    for (path, bundle_hash) in &offered {
        let recorded = previous.get(path).cloned();
        let conflict = |kind, local: Option<String>| Conflict {
            path: path.clone(),
            kind,
            local_hash: local,
            recorded_hash: recorded.clone(),
            bundle_hash: Some(bundle_hash.clone()),
            bundle_file: Some(bundle_dir.join(path).to_string_lossy().into_owned()),
        };
        let Ok(target) = crate::inspect::safe_write_target(checkout, path) else {
            report.conflicts.push(conflict(ConflictKind::Unsafe, None));
            continue;
        };
        let local = match local_hash(&target) {
            Ok(local) => local,
            Err(()) => {
                report
                    .conflicts
                    .push(conflict(ConflictKind::Untracked, None));
                continue;
            }
        };

        let write = match (&local, &recorded) {
            (Some(local), _) if local == bundle_hash => {
                if recorded.as_deref() == Some(bundle_hash.as_str()) {
                    report.unchanged += 1;
                } else {
                    report.adopted.push(path.clone());
                }
                files.insert(path.clone(), bundle_hash.clone());
                false
            }
            (None, _) => {
                report.written.push(path.clone());
                true
            }
            (Some(local), Some(recorded)) if local == recorded => {
                report.updated.push(path.clone());
                true
            }
            (Some(local), Some(recorded)) => {
                report
                    .conflicts
                    .push(conflict(ConflictKind::Edited, Some(local.clone())));
                // Kept as it was: the file is still the one this module last wrote plus somebody's
                // edit, and the next run must go on seeing it as edited rather than as untracked.
                files.insert(path.clone(), recorded.clone());
                false
            }
            (Some(local), None) => {
                report
                    .conflicts
                    .push(conflict(ConflictKind::Untracked, Some(local.clone())));
                false
            }
        };

        if !write || mode == Mode::Preview {
            continue;
        }
        let written = if outcome.is_ok() {
            std::fs::read(bundle_dir.join(path))
                .and_then(|bytes| {
                    // Hashed again from the bytes actually written: the bundle could change between
                    // the walk and the copy, and the record must describe the file on disk.
                    let hash = workflows::hash_of(&bytes);
                    crate::project_state::write_bytes_atomically(&target, &bytes).map(|()| hash)
                })
                .map_err(|error| format!("{path}: {error}"))
        } else {
            Err(String::new())
        };
        match written {
            Ok(hash) => {
                files.insert(path.clone(), hash);
            }
            Err(error) => {
                if outcome.is_ok() {
                    outcome = Err(error);
                }
                // Not written, so what this module last wrote there is still what it last wrote.
                // Dropping it would make the untouched file read as somebody's on the retry.
                if let Some(recorded) = recorded {
                    files.insert(path.clone(), recorded);
                }
            }
        }
    }

    if let Some(plan) = plan_block(
        checkout,
        block.as_ref(),
        previous.get(AGENTS_BLOCK_KEY).map(String::as_str),
        &mut report,
    ) {
        let mut recorded = plan.keep;
        if let (Some((contents, after)), Mode::Apply) = (plan.change, mode) {
            let applied = if outcome.is_ok() {
                match &contents {
                    Some(text) => {
                        crate::project_state::write_bytes_atomically(&plan.target, text.as_bytes())
                    }
                    None => std::fs::remove_file(&plan.target),
                }
                .map_err(|error| format!("{AGENTS_FILE}: {error}"))
            } else {
                Err(String::new())
            };
            match applied {
                Ok(()) => recorded = after,
                Err(error) => {
                    if outcome.is_ok() {
                        outcome = Err(error);
                    }
                }
            }
        }
        if let Some(hash) = recorded {
            files.insert(AGENTS_BLOCK_KEY.to_string(), hash);
        }
    }

    // What the previous version provided and this one does not. The block's key is not a path and
    // was decided above.
    for (path, recorded) in &previous {
        if offered.contains_key(path) || reserved(path) || path == AGENTS_BLOCK_KEY {
            continue;
        }
        let Ok(target) = crate::inspect::safe_write_target(checkout, path) else {
            continue;
        };
        match local_hash(&target) {
            Ok(None) => {}
            Ok(Some(local)) if &local == recorded => {
                report.deleted.push(path.clone());
                if mode == Mode::Apply {
                    let removed = if outcome.is_ok() {
                        std::fs::remove_file(&target).map_err(|error| format!("{path}: {error}"))
                    } else {
                        Err(String::new())
                    };
                    match removed {
                        Ok(()) => prune_empty_parents(checkout, &target),
                        Err(error) => {
                            if outcome.is_ok() {
                                outcome = Err(error);
                            }
                            files.insert(path.clone(), recorded.clone());
                        }
                    }
                }
            }
            Ok(local) => {
                report.conflicts.push(Conflict {
                    path: path.clone(),
                    kind: ConflictKind::RemovedEdited,
                    local_hash: local,
                    recorded_hash: Some(recorded.clone()),
                    bundle_hash: None,
                    bundle_file: None,
                });
                // Kept with the hash this module last wrote, not dropped: the file is still ours
                // plus somebody's edit, and every later run must go on reporting it — and may
                // still remove it once the edit is reverted to what was written.
                files.insert(path.clone(), recorded.clone());
            }
            Err(()) => {}
        }
    }

    if mode == Mode::Apply {
        if files.is_empty() {
            record.bundles.remove(&bundle.name);
        } else {
            record.bundles.insert(
                bundle.name.clone(),
                RecordedBundle {
                    version: bundle.version.clone(),
                    files,
                },
            );
        }
        // Written even when a file failed, and before the failure is returned: every file already
        // written is recorded, so the retry sees them as this module's and not as somebody's edits.
        if record != before {
            crate::project_state::write_atomically(record_path, &render_record(&record))
                .map_err(|error| format!("{}: {error}", record_path.display()))?;
        }
    }
    outcome.map(|()| report)
}

/* ---------------------------------------------------------- unmaterialize -- */

/// Why a recorded file was left where it is when its workflow stopped being used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptReason {
    /// Not what this module wrote: somebody edited it, and it is theirs now.
    Modified,
    /// Already gone — the file, or the markers of the managed block.
    Missing,
    /// The recorded path is not a relative path inside the checkout, or resolves out of it
    /// through a link or a junction. Never touched.
    Unsafe,
    /// Something other than a file is there now: a directory, or a link.
    NotAFile,
    /// It was ours and unchanged, and removing it failed.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Kept {
    pub path: String,
    pub reason: KeptReason,
}

/// What stopping a workflow took out of a checkout, and what it left.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Removal {
    pub removed: Vec<String>,
    pub kept: Vec<Kept>,
}

/// Take out of `checkout` what bundle `name` put there, by the record alone, and forget it.
///
/// Every file the record says this bundle wrote goes when it is still exactly what was written,
/// and directories left empty by that go with it — never `checkout` itself, never a directory
/// holding anything else. An edited file, a missing one, and a recorded path that is not a
/// relative path inside the checkout (or that a link or junction carries out of it) are left
/// alone and reported. The managed block in `AGENTS.md` is taken out, with the blank line that
/// was put after it, when the region between the markers is unchanged; the file goes too when
/// nothing else is left in it. Then the bundle's entries leave the record, kept ones included:
/// the workflow is no longer used, and a kept file is the project's from now on.
///
/// **One checkout only.** The caller names the checkout and its record; stopping a workflow
/// cleans the project's own root, and a worktree synced from it keeps its copies and its record
/// until it is synced again or removed.
///
/// An unreadable record stops it before anything is touched, as it stops [`materialize`].
pub fn unmaterialize(checkout: &Path, record_path: &Path, name: &str) -> Result<Removal, String> {
    let mut record = read_record(record_path)?;
    let Some(recorded) = record.bundles.remove(name) else {
        return Ok(Removal::default());
    };
    let mut removal = Removal::default();
    for (path, hash) in &recorded.files {
        let outcome = if path == AGENTS_BLOCK_KEY {
            remove_block(checkout, hash)
        } else {
            remove_file(checkout, path, hash)
        };
        match outcome {
            None => removal.removed.push(path.clone()),
            Some(reason) => removal.kept.push(Kept {
                path: path.clone(),
                reason,
            }),
        }
    }
    crate::project_state::write_atomically(record_path, &render_record(&record))
        .map_err(|error| format!("{}: {error}", record_path.display()))?;
    Ok(removal)
}

/// Remove one recorded file when it is still what was written. `None` when it went.
fn remove_file(checkout: &Path, path: &str, recorded: &str) -> Option<KeptReason> {
    // The record is a file on disk and can say anything: only the one spelling of a relative
    // path inside the checkout is ever joined, and the daemon's own files are never a bundle's.
    if crate::ownership::normalise(path).as_deref() != Some(path) || reserved(path) {
        return Some(KeptReason::Unsafe);
    }
    let Ok(target) = crate::inspect::safe_write_target(checkout, path) else {
        return Some(KeptReason::Unsafe);
    };
    match local_hash(&target) {
        Ok(None) => Some(KeptReason::Missing),
        Err(()) => Some(KeptReason::NotAFile),
        Ok(Some(local)) if local != recorded => Some(KeptReason::Modified),
        Ok(Some(_)) => match std::fs::remove_file(&target) {
            Ok(()) => {
                prune_empty_parents(checkout, &target);
                None
            }
            Err(error) => {
                tracing::warn!(%error, path, "a workflow file could not be removed");
                Some(KeptReason::Failed)
            }
        },
    }
}

/// Take the managed block out of `AGENTS.md` when it is still what was written. `None` when it went.
fn remove_block(checkout: &Path, recorded: &str) -> Option<KeptReason> {
    let Ok(target) = crate::inspect::safe_write_target(checkout, AGENTS_FILE) else {
        return Some(KeptReason::Unsafe);
    };
    match std::fs::symlink_metadata(&target) {
        Err(_) => return Some(KeptReason::Missing),
        Ok(meta) if !meta.is_file() => return Some(KeptReason::NotAFile),
        Ok(_) => {}
    }
    // Not text: this module wrote text, so whatever is there now is somebody else's.
    let Some(text) = std::fs::read(&target)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
    else {
        return Some(KeptReason::Modified);
    };
    let range = match find_region(&text) {
        Region::Absent => return Some(KeptReason::Missing),
        Region::Unclosed => return Some(KeptReason::Modified),
        Region::At(range) => range,
    };
    if region_hash(&text[range.clone()]) != recorded {
        return Some(KeptReason::Modified);
    }
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let applied = match without_region(&text, range, eol) {
        Some(left) => crate::project_state::write_bytes_atomically(&target, left.as_bytes()),
        None => std::fs::remove_file(&target),
    };
    match applied {
        Ok(()) => None,
        Err(error) => {
            tracing::warn!(%error, "the managed block could not be taken out of AGENTS.md");
            Some(KeptReason::Failed)
        }
    }
}

/* ------------------------------------------------------------------- pins -- */

/// What happened to one pinned workflow when a checkout was synced.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PinOutcome {
    pub name: String,
    pub version: String,
    /// Why nothing was materialized, when nothing was. See [`source_of`].
    pub skipped: Option<String>,
    pub report: Option<Report>,
    /// A failure after the run started. The checkout may be partly written; the record says which.
    pub error: Option<String>,
}

impl PinOutcome {
    /// Whether this checkout is left without the workflow it pins, which is what a feed line has to
    /// say. An adopted workflow is not: it is the project's own folder, there is nothing to put.
    pub fn is_missing(&self) -> bool {
        self.error.is_some()
            || self
                .skipped
                .as_deref()
                .is_some_and(|why| !why.starts_with("adopted"))
    }
}

/// Where a pin's files come from, or why they come from nowhere.
///
/// - **Adopted** (`pin.path`): the project's own folder, already where it is read. Nothing to do.
/// - **Ejected**, with its copy present in `checkout`: the copy. Ejecting is taking the files into
///   the project, so the project's copy — not the library's — is what it runs.
/// - Otherwise **the library** at the pinned version, and only when it still hashes to the pin:
///   a drifted bundle is bytes nobody pinned, and these files govern what an agent does.
pub fn source_of(
    checkout: &Path,
    pin: &workflows::Pin,
    library_root: &Path,
) -> Result<workflows::Bundle, String> {
    if pin.path.is_some() {
        return Err("adopted: the project's own folder is already in place".to_string());
    }
    if pin.ejected_at.is_some()
        && let Some(copy) = workflows::copy_path(checkout, pin).filter(|dir| dir.is_dir())
    {
        return workflows::read_bundle(&copy, &pin.name, &pin.version)
            .map_err(io)?
            .ok_or_else(|| "ejected: the project's copy has no bundle.yaml".to_string());
    }
    let dir = library_root.join(&pin.name).join(&pin.version);
    let bundle = workflows::read_bundle(&dir, &pin.name, &pin.version)
        .map_err(io)?
        .ok_or_else(|| format!("missing: the library has no {}@{}", pin.name, pin.version))?;
    if bundle.hash != pin.hash {
        return Err(format!(
            "drifted: the library's {}@{} is not what was pinned",
            pin.name, pin.version
        ));
    }
    Ok(bundle)
}

/// Materialize every workflow `pins_file` pins — or only `only`, when given — into `checkout`.
pub fn sync(
    checkout: &Path,
    pins_file: &Path,
    library_root: &Path,
    record_path: &Path,
    only: Option<&str>,
    mode: Mode,
) -> Result<Vec<PinOutcome>, String> {
    let pins = workflows::read_pins(pins_file)?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for pin in pins.workflows {
        if only.is_some_and(|name| name != pin.name) {
            continue;
        }
        seen.insert(pin.name.clone());
        let mut outcome = PinOutcome {
            name: pin.name.clone(),
            version: pin.version.clone(),
            skipped: None,
            report: None,
            error: None,
        };
        match source_of(checkout, &pin, library_root) {
            Err(why) => outcome.skipped = Some(why),
            Ok(bundle) => match materialize(&bundle, checkout, record_path, mode) {
                Ok(report) => outcome.report = Some(report),
                Err(error) => outcome.error = Some(error),
            },
        }
        out.push(outcome);
    }
    if let Some(name) = only
        && !seen.contains(name)
    {
        return Err(format!("this project does not use {name}"));
    }
    Ok(out)
}

/* --------------------------------------------------------------- worktrees -- */

/// Materialize a project's pinned workflows into a worktree the daemon just created.
///
/// **Fail-soft, and loudly so.** A job whose tree lacks its workflow still has its code, its task
/// and its gate; failing it would trade a degraded run for none. But a run without its rules looks
/// exactly like a run nobody gave rules to, so every pin that did not arrive is a warning in the
/// log and a `worktree_workflow_missing` line in the feed, naming what is missing and why.
///
/// A project with no state directory, no pins or no library is a project using no workflow, which
/// is not a failure and says nothing.
pub async fn into_worktree(
    pool: &sqlx::SqlitePool,
    machine_root: Option<&Path>,
    library_root: Option<&Path>,
    project_id: &str,
    worktree: &Path,
) {
    let (Some(machine_root), Some(library_root)) = (machine_root, library_root) else {
        return;
    };
    // `integration-*` trees are never synced, by decision rather than by omission. They are the
    // daemon's own merge-computation checkouts (`git_exec::integration_worktree`): no agent runs
    // in one, `prepare_integration_worktree` resets and cleans it before every operation, and the
    // gate that measures a merge there must measure the merge commit as it is, not the commit plus
    // untracked rules this module put beside it. No caller passes one today — they are opened by
    // `git_exec`, never by `worktree::create_at` — and this guard keeps it that way if one ever does.
    if worktree.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .starts_with(crate::git_exec::INTEGRATION_PREFIX)
    }) {
        return;
    }
    let Some(state_dir) = crate::project_state::dir(machine_root, project_id) else {
        return;
    };
    let pins_file = state_dir.join(crate::project_state::PINS_FILE);
    if !pins_file.is_file() {
        return;
    }
    let record = worktree_record(&state_dir, worktree);
    let checkout = worktree.to_path_buf();
    let library = library_root.to_path_buf();
    let synced = tokio::task::spawn_blocking(move || {
        sync(&checkout, &pins_file, &library, &record, None, Mode::Apply)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);

    let missing: Vec<String> = match synced {
        Err(error) => vec![error],
        Ok(outcomes) => {
            for outcome in &outcomes {
                if let Some(report) = &outcome.report
                    && !report.conflicts.is_empty()
                {
                    tracing::warn!(
                        project_id,
                        worktree = %worktree.display(),
                        workflow = %outcome.name,
                        conflicts = report.conflicts.len(),
                        "a worktree already held edited copies of workflow files; they were left alone"
                    );
                }
            }
            outcomes
                .iter()
                .filter(|outcome| outcome.is_missing())
                .map(|outcome| {
                    format!(
                        "{}@{} ({})",
                        outcome.name,
                        outcome.version,
                        outcome
                            .error
                            .as_deref()
                            .or(outcome.skipped.as_deref())
                            .unwrap_or("unknown")
                    )
                })
                .collect()
        }
    };
    if missing.is_empty() {
        return;
    }
    let said = format!(
        "{} is missing its workflow: {}",
        worktree
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| worktree.display().to_string()),
        missing.join("; ")
    );
    tracing::warn!(project_id, worktree = %worktree.display(), "{said}");
    if let Err(error) = crate::feed::append(
        pool,
        Some(project_id),
        "worktree_workflow_missing",
        &said,
        None,
        None,
    )
    .await
    {
        tracing::error!(%error, project_id, "a worktree is missing its workflow and the feed line was not recorded");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library holding one bundle, and a checkout beside it. Returns (library, checkout, record).
    fn setup(files: &[(&str, &str)], owns: &[&str]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        shelve(temp.path(), "1.0", files, owns);
        let record = temp.path().join("state").join("materialized.yaml");
        (temp, checkout, record)
    }

    fn shelve(root: &Path, version: &str, files: &[(&str, &str)], owns: &[&str]) {
        let dir = root.join("lib").join("dev").join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = format!(
            "owns:\n{}",
            owns.iter()
                .map(|entry| format!("  - {entry}\n"))
                .collect::<String>()
        );
        std::fs::write(dir.join(workflows::MANIFEST), manifest).unwrap();
        for (path, contents) in files {
            let target = dir.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
    }

    fn bundle(root: &Path, version: &str) -> workflows::Bundle {
        workflows::read_bundle(&root.join("lib").join("dev").join(version), "dev", version)
            .unwrap()
            .unwrap()
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn a_first_run_writes_every_owned_file_and_nothing_else() {
        let (temp, checkout, record) = setup(
            &[
                (".ai/workflow/workflow.md", "the pipeline"),
                (".ai/workflow/dispatch.md", "dispatch"),
                (".claude/agents/wf-planner.md", "planner"),
                ("graph.yaml", "nodes: []"),
            ],
            &[".ai/workflow/", ".claude/agents/wf-planner.md"],
        );
        let report =
            materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();

        assert_eq!(report.written.len(), 3);
        assert!(report.conflicts.is_empty());
        assert_eq!(
            read(&checkout.join(".ai/workflow/workflow.md")),
            "the pipeline"
        );
        assert_eq!(
            read(&checkout.join(".claude/agents/wf-planner.md")),
            "planner"
        );
        // The graph and the manifest are the library's; only what `owns` names is provided.
        assert!(!checkout.join("graph.yaml").exists());
        assert!(!checkout.join(workflows::MANIFEST).exists());

        let recorded = read_record(&record).unwrap();
        assert_eq!(recorded.bundles["dev"].version, "1.0");
        assert_eq!(
            recorded.bundles["dev"].files[".ai/workflow/workflow.md"],
            workflows::hash_of(b"the pipeline")
        );
        assert!(read(&record).starts_with("# Written by NucleOS."));
    }

    /// The 2026-09-19 loss, as a test: an edited file survives a new version, and says so.
    #[test]
    fn an_edited_file_is_never_overwritten_and_an_unchanged_one_is() {
        let (temp, checkout, record) =
            setup(&[("a.md", "one"), ("b.md", "two")], &["a.md", "b.md"]);
        materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        std::fs::write(checkout.join("a.md"), "mine").unwrap();

        shelve(
            temp.path(),
            "1.1",
            &[("a.md", "ONE"), ("b.md", "TWO")],
            &["a.md", "b.md"],
        );
        let report =
            materialize(&bundle(temp.path(), "1.1"), &checkout, &record, Mode::Apply).unwrap();

        assert_eq!(read(&checkout.join("a.md")), "mine");
        assert_eq!(read(&checkout.join("b.md")), "TWO");
        assert_eq!(report.updated, vec!["b.md".to_string()]);
        assert_eq!(report.conflicts.len(), 1);
        let conflict = &report.conflicts[0];
        assert_eq!(conflict.kind, ConflictKind::Edited);
        assert_eq!(
            conflict.local_hash.as_deref(),
            Some(workflows::hash_of(b"mine").as_str())
        );
        assert_eq!(
            conflict.recorded_hash.as_deref(),
            Some(workflows::hash_of(b"one").as_str())
        );
        assert_eq!(
            conflict.bundle_hash.as_deref(),
            Some(workflows::hash_of(b"ONE").as_str())
        );
        assert!(conflict.bundle_file.as_deref().unwrap().ends_with("a.md"));

        // And it stays a conflict on the next run, rather than turning into an untracked file.
        let again =
            materialize(&bundle(temp.path(), "1.1"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(again.conflicts[0].kind, ConflictKind::Edited);
        assert_eq!(again.unchanged, 1);
    }

    #[test]
    fn a_file_there_before_is_a_conflict_unless_it_is_already_the_bundles() {
        let (temp, checkout, record) = setup(
            &[("same.md", "same"), ("other.md", "bundle")],
            &["same.md", "other.md"],
        );
        std::fs::write(checkout.join("same.md"), "same").unwrap();
        std::fs::write(checkout.join("other.md"), "somebody's").unwrap();

        let report =
            materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(report.adopted, vec!["same.md".to_string()]);
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].kind, ConflictKind::Untracked);
        assert!(report.conflicts[0].recorded_hash.is_none());
        assert_eq!(read(&checkout.join("other.md")), "somebody's");

        let recorded = read_record(&record).unwrap();
        assert!(recorded.bundles["dev"].files.contains_key("same.md"));
        assert!(!recorded.bundles["dev"].files.contains_key("other.md"));
    }

    #[test]
    fn a_file_the_new_version_dropped_is_removed_only_when_nobody_touched_it() {
        let (temp, checkout, record) = setup(
            &[("keep.md", "k"), ("gone/a.md", "a"), ("edited.md", "e")],
            &["keep.md", "gone", "edited.md"],
        );
        materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        std::fs::write(checkout.join("edited.md"), "mine").unwrap();

        shelve(temp.path(), "2.0", &[("keep.md", "k")], &["keep.md"]);
        let report =
            materialize(&bundle(temp.path(), "2.0"), &checkout, &record, Mode::Apply).unwrap();

        assert_eq!(report.deleted, vec!["gone/a.md".to_string()]);
        assert!(
            !checkout.join("gone").exists(),
            "an emptied directory goes with its file"
        );
        assert_eq!(read(&checkout.join("edited.md")), "mine");
        assert_eq!(report.conflicts[0].kind, ConflictKind::RemovedEdited);
        let recorded = read_record(&record).unwrap();
        assert_eq!(recorded.bundles["dev"].version, "2.0");
        // The edited file stays recorded with what was written, so later runs still see it as
        // edited rather than forgetting it; the removed one is gone from the record.
        assert_eq!(
            recorded.bundles["dev"].files.keys().collect::<Vec<_>>(),
            vec!["edited.md", "keep.md"]
        );
        assert_eq!(
            recorded.bundles["dev"].files["edited.md"],
            workflows::hash_of(b"e")
        );
    }

    #[test]
    fn f5_an_edited_removed_file_stays_recorded_for_later_syncs() {
        let (temp, checkout, record) = setup(&[("edited.md", "before")], &["edited.md"]);
        materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        std::fs::write(checkout.join("edited.md"), "mine").unwrap();

        shelve(temp.path(), "2.0", &[], &[]);
        let removed =
            materialize(&bundle(temp.path(), "2.0"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(removed.conflicts[0].kind, ConflictKind::RemovedEdited);

        let again =
            materialize(&bundle(temp.path(), "2.0"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(again.conflicts.len(), 1);
        assert_eq!(again.conflicts[0].kind, ConflictKind::RemovedEdited);
    }

    #[tokio::test]
    async fn f5_a_missing_workflow_is_fail_soft_and_writes_a_feed_line() {
        let pool = crate::testdb::fresh_pool().await;
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        crate::project_state::write_for_test(
            temp.path(),
            "project",
            crate::project_state::PINS_FILE,
            "workflows:\n  - name: dev\n    version: 1.0\n    hash: sha256:missing\n    origin: test\n",
        );

        into_worktree(
            &pool,
            Some(temp.path()),
            Some(&temp.path().join("library")),
            "project",
            &worktree,
        )
        .await;

        let kind: String = sqlx::query_scalar("SELECT kind FROM feed ORDER BY id DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kind, "worktree_workflow_missing");
        assert!(worktree.is_dir());
    }

    #[test]
    fn a_preview_writes_nothing_and_says_what_it_would_do() {
        let (temp, checkout, record) = setup(&[("a.md", "one")], &["a.md"]);
        let report = materialize(
            &bundle(temp.path(), "1.0"),
            &checkout,
            &record,
            Mode::Preview,
        )
        .unwrap();
        assert!(!report.applied);
        assert_eq!(report.written, vec!["a.md".to_string()]);
        assert!(!checkout.join("a.md").exists());
        assert!(!record.exists());
    }

    /// The hook the daemon installs is never a bundle's, whatever the manifest claims.
    #[test]
    fn the_files_the_daemon_installs_are_never_materialized() {
        let (temp, checkout, record) = setup(
            &[
                (".claude/settings.json", "{}"),
                (".claude/hooks/ask_daemon.py", "evil"),
                (".claude/skills/plan/SKILL.md", "plan"),
            ],
            &[".claude"],
        );
        let report =
            materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(
            report.written,
            vec![".claude/skills/plan/SKILL.md".to_string()]
        );
        assert_eq!(report.reserved.len(), 2);
        assert!(!checkout.join(".claude/settings.json").exists());
        assert!(!checkout.join(".claude/hooks").exists());
    }

    /// An entry that climbs out of the project is dropped before it can be joined onto anything.
    #[test]
    fn an_owns_entry_that_leaves_the_project_provides_nothing() {
        assert_eq!(
            owns_entries(&[
                "../outside".into(),
                "/etc".into(),
                "C:/x".into(),
                r".ai\x".into(),
                "./.ai/workflow/".into(),
            ]),
            vec![".ai/workflow".to_string()]
        );
    }

    /// Without the record an unchanged file and an edited one look the same, so a record that
    /// cannot be read stops the run instead of being guessed around.
    #[test]
    fn an_unreadable_record_stops_the_run_before_anything_is_written() {
        let (temp, checkout, record) = setup(&[("a.md", "one")], &["a.md"]);
        std::fs::create_dir_all(record.parent().unwrap()).unwrap();
        std::fs::write(&record, "bundles: [not, a, map").unwrap();
        assert!(materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).is_err());
        assert!(!checkout.join("a.md").exists());
    }

    /// A tree named by the daemon's path and by a canonical one shares one record.
    #[cfg(windows)]
    #[test]
    fn one_worktree_spelled_two_ways_keeps_one_record() {
        let state = Path::new("/state");
        let plain = worktree_record(state, Path::new(r"C:\trees\job-12"));
        assert_eq!(
            plain,
            worktree_record(state, Path::new(r"\\?\c:\Trees\job-12"))
        );
        assert_eq!(plain, worktree_record(state, Path::new("C:/trees/job-12")));
    }

    /// The daemon's merge-computation trees run no agent, so they get no workflow — and no feed
    /// line saying it is missing, which is what a pin with no library would otherwise produce.
    #[tokio::test]
    async fn an_integration_tree_is_never_synced() {
        let pool = crate::testdb::fresh_pool().await;
        let temp = tempfile::tempdir().unwrap();
        let tree = temp
            .path()
            .join(format!("{}project", crate::git_exec::INTEGRATION_PREFIX));
        std::fs::create_dir_all(&tree).unwrap();
        crate::project_state::write_for_test(
            temp.path(),
            "project",
            crate::project_state::PINS_FILE,
            "workflows:\n  - name: dev\n    version: 1.0\n    hash: sha256:missing\n    origin: test\n",
        );

        into_worktree(
            &pool,
            Some(temp.path()),
            Some(&temp.path().join("library")),
            "project",
            &tree,
        )
        .await;

        let lines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feed")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(lines, 0);
    }

    #[test]
    fn two_worktrees_with_one_name_keep_two_records() {
        let state = Path::new("/state");
        let a = worktree_record(state, Path::new("/one/feature"));
        let b = worktree_record(state, Path::new("/two/feature"));
        assert_ne!(a, b);
        assert!(a.starts_with(state.join("materialized")));
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("feature-")
        );
    }

    /// The pins decide what is synced: a drifted bundle is not, an ejected one comes from the
    /// project's copy, an adopted one is the project's own folder and needs nothing.
    #[test]
    fn syncing_follows_the_pins_and_refuses_bytes_nobody_pinned() {
        let (temp, checkout, record) = setup(&[("a.md", "one")], &["a.md"]);
        let pins = temp
            .path()
            .join("state")
            .join(crate::project_state::PINS_FILE);
        let library = temp.path().join("lib");
        workflows::install(&pins, &bundle(temp.path(), "1.0")).unwrap();

        let outcomes = sync(&checkout, &pins, &library, &record, None, Mode::Apply).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].report.as_ref().unwrap().written.len(), 1);

        std::fs::write(library.join("dev/1.0/a.md"), "tampered").unwrap();
        let outcomes = sync(&checkout, &pins, &library, &record, None, Mode::Apply).unwrap();
        assert!(
            outcomes[0]
                .skipped
                .as_deref()
                .unwrap()
                .starts_with("drifted")
        );
        assert!(outcomes[0].is_missing());
        assert_eq!(read(&checkout.join("a.md")), "one");

        assert!(
            sync(
                &checkout,
                &pins,
                &library,
                &record,
                Some("nope"),
                Mode::Preview
            )
            .is_err()
        );
    }

    /* ------------------------------------------------------- managed block -- */

    /// A source is the whole region, markers included, as the installer's `agents-block.md` is.
    const BLOCK: &str = "# >>> AI WORKFLOW MANAGED BLOCK >>>\n\n## AI workflow integration\n\nthe rules\n\n# <<< AI WORKFLOW MANAGED BLOCK <<<\n";
    const NEW_BLOCK: &str =
        "# >>> AI WORKFLOW MANAGED BLOCK >>>\n\nnew rules\n\n# <<< AI WORKFLOW MANAGED BLOCK <<<\n";

    /// Shelve a bundle whose manifest names `.ai/workflow/agents-block.md` as its managed block.
    fn shelve_block(root: &Path, version: &str, source_text: &str) {
        let source = ".ai/workflow/agents-block.md";
        shelve(root, version, &[(source, source_text)], &[".ai/workflow/"]);
        let manifest = root
            .join("lib")
            .join("dev")
            .join(version)
            .join(workflows::MANIFEST);
        let text = read(&manifest);
        std::fs::write(&manifest, format!("{text}agents_block: {source}\n")).unwrap();
    }

    fn block_setup(source_text: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let (temp, checkout, record) = setup(&[], &[]);
        shelve_block(temp.path(), "1.0", source_text);
        (temp, checkout, record)
    }

    #[test]
    fn the_managed_block_creates_a_missing_agents_file_holding_only_the_block() {
        let (temp, checkout, record) = block_setup(BLOCK);
        let report =
            materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();

        assert_eq!(read(&checkout.join("AGENTS.md")), BLOCK);
        assert!(report.written.contains(&AGENTS_BLOCK_KEY.to_string()));
        assert_eq!(
            read_record(&record).unwrap().bundles["dev"].files[AGENTS_BLOCK_KEY],
            workflows::hash_of(BLOCK.as_bytes())
        );
    }

    /// The rest of the file is the project's, byte for byte, and so are its line endings.
    #[test]
    fn the_managed_block_is_prepended_when_there_are_no_markers_and_the_rest_is_untouched() {
        let (temp, checkout, record) = block_setup(BLOCK);
        let rest = "## Project: mine\r\n\r\nits own notes\r\n";
        std::fs::write(checkout.join("AGENTS.md"), rest).unwrap();
        let report =
            materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();

        assert!(report.written.contains(&AGENTS_BLOCK_KEY.to_string()));
        assert_eq!(
            read(&checkout.join("AGENTS.md")),
            format!("{}\r\n{rest}", BLOCK.replace('\n', "\r\n"))
        );
    }

    #[test]
    fn an_unchanged_managed_block_is_replaced_by_the_new_version_and_nothing_else_moves() {
        let (temp, checkout, record) = block_setup(BLOCK);
        std::fs::write(checkout.join("AGENTS.md"), "above\n").unwrap();
        materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        // Somebody adds a line after everything, which is theirs to add.
        let now = read(&checkout.join("AGENTS.md"));
        std::fs::write(checkout.join("AGENTS.md"), format!("{now}below\n")).unwrap();

        shelve_block(temp.path(), "1.1", NEW_BLOCK);
        let report =
            materialize(&bundle(temp.path(), "1.1"), &checkout, &record, Mode::Apply).unwrap();

        assert!(report.updated.contains(&AGENTS_BLOCK_KEY.to_string()));
        assert_eq!(
            read(&checkout.join("AGENTS.md")),
            format!("{NEW_BLOCK}\nabove\nbelow\n")
        );
    }

    #[test]
    fn an_edited_managed_block_is_a_conflict_and_is_never_overwritten() {
        let (temp, checkout, record) = block_setup(BLOCK);
        materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        let edited = BLOCK.replace("the rules", "my rules");
        std::fs::write(checkout.join("AGENTS.md"), &edited).unwrap();

        shelve_block(temp.path(), "1.1", NEW_BLOCK);
        let report =
            materialize(&bundle(temp.path(), "1.1"), &checkout, &record, Mode::Apply).unwrap();

        assert_eq!(read(&checkout.join("AGENTS.md")), edited);
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].path, AGENTS_BLOCK_KEY);
        assert_eq!(report.conflicts[0].kind, ConflictKind::Edited);
        assert_eq!(
            report.conflicts[0].recorded_hash.as_deref(),
            Some(workflows::hash_of(BLOCK.as_bytes()).as_str())
        );
        // Still edited on the next run, not forgotten into an untracked block.
        let again =
            materialize(&bundle(temp.path(), "1.1"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(again.conflicts[0].kind, ConflictKind::Edited);
    }

    /// Today's hand-kept AGENTS.md: the block is already there, never recorded, and exactly what
    /// the bundle would write — so it is adopted, not reported. The source is LF and the checkout
    /// CRLF, as the installer's template and this repository's AGENTS.md are: still identical.
    #[test]
    fn an_unrecorded_block_identical_to_the_bundles_is_adopted_and_a_different_one_is_not() {
        let (temp, checkout, record) = block_setup(BLOCK);
        let file = format!("{}\r\n## Project\r\n", BLOCK.replace('\n', "\r\n"));
        std::fs::write(checkout.join("AGENTS.md"), &file).unwrap();
        let report =
            materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(report.adopted, vec![AGENTS_BLOCK_KEY.to_string()]);
        assert!(report.conflicts.is_empty());
        assert_eq!(read(&checkout.join("AGENTS.md")), file);
        assert!(
            read_record(&record).unwrap().bundles["dev"]
                .files
                .contains_key(AGENTS_BLOCK_KEY)
        );

        let (temp, checkout, record) = block_setup(BLOCK);
        let theirs = BLOCK.replace("the rules", "their rules");
        std::fs::write(checkout.join("AGENTS.md"), &theirs).unwrap();
        let report =
            materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(report.conflicts[0].kind, ConflictKind::Untracked);
        assert_eq!(read(&checkout.join("AGENTS.md")), theirs);
    }

    #[test]
    fn a_dropped_managed_block_is_removed_when_unchanged_and_kept_when_edited() {
        let (temp, checkout, record) = block_setup(BLOCK);
        std::fs::write(checkout.join("AGENTS.md"), "## Project\n").unwrap();
        materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();

        shelve(temp.path(), "2.0", &[("x.md", "x")], &["x.md"]);
        let report =
            materialize(&bundle(temp.path(), "2.0"), &checkout, &record, Mode::Apply).unwrap();
        assert!(report.deleted.contains(&AGENTS_BLOCK_KEY.to_string()));
        assert_eq!(read(&checkout.join("AGENTS.md")), "## Project\n");
        assert!(
            !read_record(&record).unwrap().bundles["dev"]
                .files
                .contains_key(AGENTS_BLOCK_KEY)
        );

        let (temp, checkout, record) = block_setup(BLOCK);
        materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        let edited = BLOCK.replace("the rules", "my rules");
        std::fs::write(checkout.join("AGENTS.md"), &edited).unwrap();
        shelve(temp.path(), "2.0", &[("x.md", "x")], &["x.md"]);
        let report =
            materialize(&bundle(temp.path(), "2.0"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(report.conflicts[0].kind, ConflictKind::RemovedEdited);
        assert_eq!(report.conflicts[0].path, AGENTS_BLOCK_KEY);
        assert_eq!(read(&checkout.join("AGENTS.md")), edited);
        assert_eq!(
            read_record(&record).unwrap().bundles["dev"].files[AGENTS_BLOCK_KEY],
            workflows::hash_of(BLOCK.as_bytes())
        );
    }

    #[test]
    fn a_preview_of_the_managed_block_writes_nothing() {
        let (temp, checkout, record) = block_setup(BLOCK);
        std::fs::write(checkout.join("AGENTS.md"), "mine\n").unwrap();
        let report = materialize(
            &bundle(temp.path(), "1.0"),
            &checkout,
            &record,
            Mode::Preview,
        )
        .unwrap();
        assert!(report.written.contains(&AGENTS_BLOCK_KEY.to_string()));
        assert_eq!(read(&checkout.join("AGENTS.md")), "mine\n");
        assert!(!record.exists());
    }

    /// A source without its markers would write a region the next run cannot find, so the run
    /// stops before any file, block or record is written — and says why.
    #[test]
    fn a_managed_block_source_without_its_markers_stops_the_run_and_writes_nothing() {
        for source in [
            "## AI workflow integration\n\nthe rules\n",
            "# >>> AI WORKFLOW MANAGED BLOCK >>>\n\nno end\n",
            "# >>> AI WORKFLOW MANAGED BLOCK >>>\n# <<< AI WORKFLOW MANAGED BLOCK <<<\nafter\n# <<< AI WORKFLOW MANAGED BLOCK <<<\n",
        ] {
            let (temp, checkout, record) = block_setup(source);
            std::fs::write(checkout.join("AGENTS.md"), "mine\n").unwrap();
            let error = materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply)
                .unwrap_err();
            assert!(error.contains("AI WORKFLOW MANAGED BLOCK"), "{error}");
            assert_eq!(read(&checkout.join("AGENTS.md")), "mine\n");
            assert!(!checkout.join(".ai/workflow/agents-block.md").exists());
            assert!(!record.exists());
        }
    }

    /// A bundle that names no block leaves AGENTS.md alone, markers and all.
    #[test]
    fn a_bundle_without_a_managed_block_never_touches_agents_md() {
        let (temp, checkout, record) = setup(&[("a.md", "one")], &["a.md"]);
        std::fs::write(checkout.join("AGENTS.md"), BLOCK).unwrap();
        let report =
            materialize(&bundle(temp.path(), "1.0"), &checkout, &record, Mode::Apply).unwrap();
        assert_eq!(report.written, vec!["a.md".to_string()]);
        assert!(report.conflicts.is_empty());
        assert_eq!(read(&checkout.join("AGENTS.md")), BLOCK);
    }
}

#[cfg(test)]
mod unmaterialize_tests {
    use super::*;

    fn checkout_with(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        for (path, contents) in files {
            let target = checkout.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        let record = temp.path().join("state").join("materialized.yaml");
        (temp, checkout, record)
    }

    /// Record `files` as what bundle `dev` wrote, beside another bundle's entry.
    fn record_as_written(record: &Path, files: &[(&str, String)]) {
        let mut written = Record::default();
        written.bundles.insert(
            "dev".to_string(),
            RecordedBundle {
                version: "1.0".to_string(),
                files: files
                    .iter()
                    .map(|(path, hash)| (path.to_string(), hash.clone()))
                    .collect(),
            },
        );
        written.bundles.insert(
            "other".to_string(),
            RecordedBundle {
                version: "2.0".to_string(),
                files: [("keep.md".to_string(), workflows::hash_of(b"k"))].into(),
            },
        );
        crate::project_state::write_atomically(record, &render_record(&written)).unwrap();
    }

    fn hash(text: &str) -> String {
        workflows::hash_of(text.as_bytes())
    }

    fn kept(removal: &Removal, path: &str) -> Option<KeptReason> {
        removal
            .kept
            .iter()
            .find(|kept| kept.path == path)
            .map(|kept| kept.reason)
    }

    const BLOCK: &str =
        "# >>> AI WORKFLOW MANAGED BLOCK >>>\n\nthe rules\n\n# <<< AI WORKFLOW MANAGED BLOCK <<<\n";

    #[test]
    fn untouched_files_go_edited_and_missing_ones_stay_and_empty_dirs_are_pruned() {
        let (_temp, checkout, record) = checkout_with(&[
            ("a/x.md", "x"),
            ("a/y.md", "mine"),
            ("b/deep/z.md", "z"),
            ("c/w.md", "w"),
            ("c/theirs.txt", "not ours"),
        ]);
        record_as_written(
            &record,
            &[
                ("a/x.md", hash("x")),
                ("a/y.md", hash("y")),
                ("b/deep/z.md", hash("z")),
                ("c/w.md", hash("w")),
                ("d.md", hash("d")),
            ],
        );

        let removal = unmaterialize(&checkout, &record, "dev").unwrap();

        assert!(!checkout.join("a/x.md").exists());
        assert_eq!(
            std::fs::read_to_string(checkout.join("a/y.md")).unwrap(),
            "mine"
        );
        assert!(
            checkout.join("a").is_dir(),
            "a directory still holding a file stays"
        );
        assert!(!checkout.join("b").exists(), "directories left empty go");
        assert!(!checkout.join("c/w.md").exists());
        assert!(checkout.join("c/theirs.txt").is_file());
        assert!(checkout.is_dir(), "never the root");

        assert_eq!(
            removal.removed,
            vec![
                "a/x.md".to_string(),
                "b/deep/z.md".to_string(),
                "c/w.md".to_string()
            ]
        );
        assert_eq!(kept(&removal, "a/y.md"), Some(KeptReason::Modified));
        assert_eq!(kept(&removal, "d.md"), Some(KeptReason::Missing));
        assert_eq!(removal.kept.len(), 2);

        // The bundle's entries are gone, kept ones included; another bundle's are not.
        let after = read_record(&record).unwrap();
        assert!(!after.bundles.contains_key("dev"));
        assert!(after.bundles.contains_key("other"));
    }

    #[test]
    fn an_unchanged_block_goes_and_the_rest_of_agents_md_is_byte_identical() {
        let rest = "## Project: mine\r\n\r\nits own notes\r\n";
        let (_temp, checkout, record) = checkout_with(&[(
            "AGENTS.md",
            &format!("{}\r\n{rest}", BLOCK.replace('\n', "\r\n")),
        )]);
        record_as_written(&record, &[(AGENTS_BLOCK_KEY, hash(BLOCK))]);

        let removal = unmaterialize(&checkout, &record, "dev").unwrap();

        assert_eq!(removal.removed, vec![AGENTS_BLOCK_KEY.to_string()]);
        assert!(removal.kept.is_empty());
        assert_eq!(
            std::fs::read_to_string(checkout.join("AGENTS.md")).unwrap(),
            rest
        );
    }

    #[test]
    fn an_agents_md_holding_only_the_block_is_deleted() {
        let (_temp, checkout, record) = checkout_with(&[("AGENTS.md", BLOCK)]);
        record_as_written(&record, &[(AGENTS_BLOCK_KEY, hash(BLOCK))]);

        let removal = unmaterialize(&checkout, &record, "dev").unwrap();

        assert_eq!(removal.removed, vec![AGENTS_BLOCK_KEY.to_string()]);
        assert!(!checkout.join("AGENTS.md").exists());
    }

    #[test]
    fn an_edited_block_is_kept_and_reported() {
        let edited = format!("{}\nbelow\n", BLOCK.replace("the rules", "my rules"));
        let (_temp, checkout, record) = checkout_with(&[("AGENTS.md", &edited)]);
        record_as_written(&record, &[(AGENTS_BLOCK_KEY, hash(BLOCK))]);

        let removal = unmaterialize(&checkout, &record, "dev").unwrap();

        assert!(removal.removed.is_empty());
        assert_eq!(kept(&removal, AGENTS_BLOCK_KEY), Some(KeptReason::Modified));
        assert_eq!(
            std::fs::read_to_string(checkout.join("AGENTS.md")).unwrap(),
            edited
        );
    }

    #[test]
    fn a_block_whose_markers_are_gone_is_reported_missing() {
        let (_temp, checkout, record) = checkout_with(&[("AGENTS.md", "just notes\n")]);
        record_as_written(&record, &[(AGENTS_BLOCK_KEY, hash(BLOCK))]);

        let removal = unmaterialize(&checkout, &record, "dev").unwrap();

        assert_eq!(kept(&removal, AGENTS_BLOCK_KEY), Some(KeptReason::Missing));
        assert_eq!(
            std::fs::read_to_string(checkout.join("AGENTS.md")).unwrap(),
            "just notes\n"
        );
    }

    /// A record is a file on disk and can say anything; nothing outside the checkout is touched.
    #[test]
    fn a_record_path_escaping_the_checkout_is_refused_and_nothing_outside_is_touched() {
        let (temp, checkout, record) = checkout_with(&[]);
        let outside = temp.path().join("outside.md");
        std::fs::write(&outside, "o").unwrap();
        let absolute = outside.to_string_lossy().into_owned();
        record_as_written(
            &record,
            &[
                ("../outside.md", hash("o")),
                ("a/../../outside.md", hash("o")),
                (absolute.as_str(), hash("o")),
            ],
        );

        let removal = unmaterialize(&checkout, &record, "dev").unwrap();

        assert!(outside.is_file());
        assert!(removal.removed.is_empty());
        assert_eq!(removal.kept.len(), 3);
        assert!(
            removal
                .kept
                .iter()
                .all(|kept| kept.reason == KeptReason::Unsafe)
        );
        assert!(!read_record(&record).unwrap().bundles.contains_key("dev"));
    }

    /// A link inside the checkout pointing out of it is never followed to delete what it names.
    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_checkout_is_never_followed() {
        let (temp, checkout, record) = checkout_with(&[]);
        let outside = temp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("f.md"), "f").unwrap();
        std::os::unix::fs::symlink(&outside, checkout.join("linked")).unwrap();
        record_as_written(&record, &[("linked/f.md", hash("f"))]);

        let removal = unmaterialize(&checkout, &record, "dev").unwrap();

        assert!(outside.join("f.md").is_file());
        assert_eq!(kept(&removal, "linked/f.md"), Some(KeptReason::Unsafe));
    }

    #[test]
    fn a_bundle_never_recorded_removes_nothing_and_writes_no_record() {
        let (_temp, checkout, record) = checkout_with(&[("a.md", "a")]);
        let removal = unmaterialize(&checkout, &record, "dev").unwrap();
        assert!(removal.removed.is_empty() && removal.kept.is_empty());
        assert!(!record.exists());
        assert!(checkout.join("a.md").is_file());
    }
}
