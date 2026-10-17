//! §spec motor-de-workflows
//!
//! Make a library bundle out of a folder that already holds a workflow:
//! `nucleos-core --workflow-package <name> <version> --from <project root> --manifest <bundle.yaml>`,
//! or out of a tracked source tree laid out as the bundle itself, with
//! `--source <dir>` in place of the last two ([`package_source`]).
//!
//! The library (`workflows.rs`) could list, pin, eject and drift bundles, and the only way one got
//! INTO it was the daemon seeding its own autopilot. A workflow a person already works by — this
//! repository's `.ai/` harness is the case in point — had no way in short of copying files by hand
//! into a directory whose layout nothing documented.
//!
//! # The manifest says what the workflow is, and nothing here second-guesses it
//!
//! The caller hands over a `bundle.yaml` whose `owns:` list names the project paths the workflow
//! consists of — the same list `workflows/materialize.rs` later puts back into a checkout. This
//! module copies exactly those paths out of the project, under the same relative names, and writes
//! the manifest beside them byte for byte, comments included. It does not know what a skill or a
//! packet is; which files are the workflow and which are one project's state is the manifest
//! author's to decide.
//!
//! # A version is written once
//!
//! Refused when `<name>/<version>/` already exists. A version is defined by its hash (`seed.rs`
//! makes the same argument for the autopilot), and packaging over one would change what every pin
//! to it means without any pin knowing.
//!
//! # The library keeps a history, because the workflow has no repository of its own
//!
//! `~/.nucleos/workflows/<name>/` becomes a git repository on first package, and every version is
//! one commit. A workflow extracted from a project has left that project's history; this is where
//! its next history starts. Local only — no remote, no network. When git is not there, or the
//! commit fails, the bundle is still packaged and the answer says why it has no commit: the files
//! are the product, and the history is a record of them.

use std::path::{Path, PathBuf};

use crate::workflows;

/// What packaging did.
#[derive(Debug, Clone, PartialEq)]
pub struct Packaged {
    /// The new version directory.
    pub dir: PathBuf,
    /// How many files were copied into it, the manifest not counted.
    pub files: usize,
    /// The bundle's hash, as a pin to it will record.
    pub hash: String,
    /// The commit that records this version, or why there is none.
    pub commit: Result<String, String>,
}

#[derive(serde::Deserialize, Default)]
struct Owns {
    #[serde(default)]
    owns: Vec<String>,
    /// The managed block's source (`workflow_materialize`), which must be among what is copied.
    agents_block: Option<String>,
    /// The bundle's hook declarations (`workflows::hooks`), raw until they are parsed below.
    #[serde(default)]
    hooks: serde_yaml::Value,
}

/// Whether `name` is an interpreter's cache rather than something an author wrote. Running a
/// bundle's own script tests leaves `__pycache__/` beside them, and copying it would make the
/// version's hash depend on which Python last ran there.
fn is_cache(name: &std::ffi::OsStr) -> bool {
    name == "__pycache__" || Path::new(name).extension().is_some_and(|ext| ext == "pyc")
}

/// Copy one file or directory tree, files only, skipping symlinks for the reason
/// `workflows::copy_tree` gives: following one would copy whatever it points at into the library.
fn copy_into(from: &Path, to: &Path, count: &mut usize) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(from)?.file_type();
    if kind.is_symlink() || from.file_name().is_some_and(is_cache) {
        return Ok(());
    }
    if kind.is_dir() {
        std::fs::create_dir_all(to)?;
        let mut entries: Vec<_> = std::fs::read_dir(from)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            copy_into(&entry.path(), &to.join(entry.file_name()), count)?;
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to)?;
    *count += 1;
    Ok(())
}

/// Package `project_root`'s workflow, as `manifest` describes it, into `library_root` as
/// `name@version`.
///
/// Everything is checked before anything is written: the names, the version not existing yet,
/// every `owns:` entry normalising and existing in the project. A half-written version directory
/// would be a bundle in the library that hashes to something nobody packaged, so a failure while
/// copying removes the directory again.
pub async fn package(
    library_root: &Path,
    name: &str,
    version: &str,
    project_root: &Path,
    manifest: &Path,
) -> Result<Packaged, String> {
    if !workflows::valid_name(name) {
        return Err(format!("`{name}` is not a usable workflow name"));
    }
    if !workflows::valid_name(version) {
        return Err(format!("`{version}` is not a usable version"));
    }
    let text = std::fs::read_to_string(manifest)
        .map_err(|error| format!("{}: {error}", manifest.display()))?;
    let Owns {
        owns,
        agents_block,
        hooks,
    } = serde_yaml::from_str::<Option<Owns>>(&text)
        .map_err(|error| format!("{}: {error}", manifest.display()))?
        .unwrap_or_default();
    if owns.is_empty() {
        return Err(format!(
            "{} names no files under `owns:`, so there is nothing to package",
            manifest.display()
        ));
    }
    let mut entries = Vec::new();
    for entry in &owns {
        let Some(rel) = crate::ownership::normalise(entry) else {
            return Err(format!(
                "`{entry}` is not a relative path inside the project (no `..`, nothing absolute, \
                 forward slashes)"
            ));
        };
        let source = project_root.join(&rel);
        if std::fs::symlink_metadata(&source).is_err() {
            return Err(format!("`{rel}` is not in {}", project_root.display()));
        }
        entries.push(rel);
    }
    // Only what `owns:` names is copied, so a block source outside it would ship a bundle whose
    // manifest points at nothing — and materializing that stops every run it is pinned into.
    if let Some(source) = &agents_block {
        let covered = crate::ownership::normalise(source).filter(|rel| {
            crate::workflows::materialize::provided(&entries, rel)
                && std::fs::symlink_metadata(project_root.join(rel))
                    .is_ok_and(|meta| meta.is_file())
        });
        let Some(rel) = covered else {
            return Err(format!(
                "agents_block `{source}` must be a file in the project under one of the `owns:` \
                 entries, or it is not packaged"
            ));
        };
        // The source is the whole region, markers included; one without them would ship a bundle
        // every materialization of which stops (`workflow_materialize::render_block`).
        let framed = std::fs::read(project_root.join(&rel))
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| crate::workflows::materialize::render_block(&text));
        if framed.is_none() {
            return Err(format!(
                "agents_block `{source}` must be UTF-8 text whose first line is the begin marker \
                 and last line the end marker of the AI WORKFLOW MANAGED BLOCK, with neither in \
                 between"
            ));
        }
    }

    // A hook script is copied into the library whether or not `owns:` names it, and when it does
    // not, it is in the library ONLY: materialization provides what `owns:` lists and nothing else,
    // so a gate kept out of that list never lands in a checkout an agent could edit it in. Checked
    // here, against the project, because a hook naming a file the bundle does not have would
    // package fine and then be skipped on every event it was meant to watch.
    let hooks = crate::workflows::hooks::parse(hooks)
        .and_then(|hooks| {
            crate::workflows::hooks::validate(&hooks, |rel| {
                std::fs::symlink_metadata(project_root.join(rel)).is_ok_and(|meta| meta.is_file())
            })
            .map(|()| hooks)
        })
        .map_err(|error| format!("{}: {error}", manifest.display()))?;
    let mut hook_scripts: Vec<String> = Vec::new();
    for hook in &hooks {
        if let Some(rel) = crate::ownership::normalise(&hook.script)
            && !crate::workflows::materialize::provided(&entries, &rel)
            && !hook_scripts.contains(&rel)
        {
            hook_scripts.push(rel);
        }
    }

    let home = library_root.join(name);
    let dir = home.join(version);
    if dir.exists() {
        return Err(format!(
            "{name}@{version} is already in the library; a version is written once — package a new one"
        ));
    }

    let copied = (|| -> std::io::Result<usize> {
        let mut count = 0;
        for rel in entries.iter().chain(&hook_scripts) {
            copy_into(&project_root.join(rel), &dir.join(rel), &mut count)?;
        }
        std::fs::write(dir.join(workflows::MANIFEST), &text)?;
        Ok(count)
    })();
    let files = match copied {
        Ok(files) => files,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("copying into {}: {error}", dir.display()));
        }
    };
    let hash = workflows::digest_of(
        &workflows::file_hashes(&dir).map_err(|error| format!("{}: {error}", dir.display()))?,
    );
    let commit = commit_version(&home, name, version).await;
    Ok(Packaged {
        dir,
        files,
        hash,
        commit,
    })
}

/* ------------------------------------------------------ from a source tree -- */

/// The Claude skills tree, which the Codex mirror below is generated from at packaging.
const CLAUDE_SKILLS: &str = ".claude/skills";
/// The skills mirror a Codex controller reads. Generated, never authored: two authored copies of
/// one skill are two skills free to disagree.
const AGENTS_SKILLS: &str = ".agents/skills";
/// The phase cockpits. Only `wf-*.md` here is a cockpit; anything else is not this check's.
const COCKPITS: &str = ".claude/agents";
/// How many offending lines a refusal lists before it stops: enough to fix a batch, short enough
/// to read.
const MAX_NAMED: usize = 20;

/// Package a bundle whose source is a directory laid out as the bundle itself, into
/// `library_root` as `name@version`:
/// `nucleos-core --workflow-package <name> <version> --source <dir>`.
///
/// The directory holds `bundle.yaml` at its top, the files to materialize at the paths they take
/// in a project, and whatever else the bundle carries (`scripts/`), which is copied into the
/// library and never materialized because `owns:` does not name it. [`package`] copies a workflow
/// OUT of a project that runs it; this is the way in for a workflow whose tracked source is not
/// any project's live tree (`core/assets/nucleos-dev/`).
///
/// So it also does what that project used to do for itself: it generates `.agents/skills/` from
/// `.claude/skills/`, and checks what the project's own gates checked — the cockpits' and skills'
/// frontmatter — plus the one thing a bundle meant for any project must not do, which is point
/// into a `.ai/` that project does not have.
///
/// **Checked as it will be published.** The tree is copied into a staging directory beside the
/// version, the mirror generated there, every check run there, and only then renamed into place.
/// Checking the source and copying after would validate a tree that is not the one shipped — the
/// generated mirror is in neither the source nor the manifest author's head — and a failure leaves
/// no version behind.
pub async fn package_source(
    library_root: &Path,
    name: &str,
    version: &str,
    source: &Path,
) -> Result<Packaged, String> {
    if !workflows::valid_name(name) {
        return Err(format!("`{name}` is not a usable workflow name"));
    }
    if !workflows::valid_name(version) {
        return Err(format!("`{version}` is not a usable version"));
    }
    let manifest = source.join(workflows::MANIFEST);
    let text = std::fs::read_to_string(&manifest)
        .map_err(|error| format!("{}: {error}", manifest.display()))?;
    workflows::parse_manifest(&text).map_err(|error| format!("{}: {error}", manifest.display()))?;
    let Owns {
        owns,
        agents_block,
        hooks,
    } = serde_yaml::from_str::<Option<Owns>>(&text)
        .map_err(|error| format!("{}: {error}", manifest.display()))?
        .unwrap_or_default();
    if owns.is_empty() {
        return Err(format!(
            "{} names no files under `owns:`, so there is nothing to materialize",
            manifest.display()
        ));
    }
    if std::fs::symlink_metadata(source.join(AGENTS_SKILLS)).is_ok() {
        return Err(format!(
            "{AGENTS_SKILLS}/ is generated from {CLAUDE_SKILLS}/ at packaging; remove it from {}",
            source.display()
        ));
    }

    let home = library_root.join(name);
    let dir = home.join(version);
    if dir.exists() {
        return Err(format!(
            "{name}@{version} is already in the library; a version is written once — package a new one"
        ));
    }
    // Dot-named, so `workflows::library` never lists it: `valid_name` refuses a leading dot.
    let staging = home.join(format!(".{version}.staging"));
    let _ = std::fs::remove_dir_all(&staging);
    let staged = (|| -> Result<usize, String> {
        let mut count = 0;
        copy_into(source, &staging, &mut count)
            .map_err(|error| format!("copying into {}: {error}", staging.display()))?;
        count += mirror_skills(&staging)?;
        check_source(&staging, &owns, agents_block.as_deref(), hooks)?;
        std::fs::rename(&staging, &dir)
            .map_err(|error| format!("moving into {}: {error}", dir.display()))?;
        // The manifest came along with the tree; [`Packaged::files`] does not count it.
        Ok(count.saturating_sub(1))
    })();
    let files = match staged {
        Ok(files) => files,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            // Only when empty: a first version that failed leaves no name behind in the listing,
            // and a name with other versions (or its history) keeps them.
            let _ = std::fs::remove_dir(&home);
            return Err(error);
        }
    };
    let hash = workflows::digest_of(
        &workflows::file_hashes(&dir).map_err(|error| format!("{}: {error}", dir.display()))?,
    );
    let commit = commit_version(&home, name, version).await;
    Ok(Packaged {
        dir,
        files,
        hash,
        commit,
    })
}

/// Copy every skill under `.claude/skills/` to `.agents/skills/`, whole. Returns the files copied.
fn mirror_skills(root: &Path) -> Result<usize, String> {
    let from = root.join(CLAUDE_SKILLS);
    let Ok(skills) = std::fs::read_dir(&from) else {
        return Ok(0);
    };
    let mut count = 0;
    for skill in skills {
        let skill = skill.map_err(|error| format!("{}: {error}", from.display()))?;
        if !skill.path().is_dir() {
            continue;
        }
        copy_into(
            &skill.path(),
            &root.join(AGENTS_SKILLS).join(skill.file_name()),
            &mut count,
        )
        .map_err(|error| format!("mirroring {}: {error}", skill.path().display()))?;
    }
    Ok(count)
}

/// Every check a source tree must pass, run against the staged copy at `root`.
fn check_source(
    root: &Path,
    owns: &[String],
    agents_block: Option<&str>,
    hooks: serde_yaml::Value,
) -> Result<(), String> {
    let mut entries = Vec::new();
    for entry in owns {
        let Some(rel) = crate::ownership::normalise(entry) else {
            return Err(format!(
                "`{entry}` is not a relative path inside the bundle (no `..`, nothing absolute, \
                 forward slashes)"
            ));
        };
        if std::fs::symlink_metadata(root.join(&rel)).is_err() {
            return Err(format!("`{rel}` is under `owns:` and not in the bundle"));
        }
        entries.push(rel);
    }
    // The daemon writes the pointer per checkout (`materialize::place_project_pointer`); a bundle
    // shipping one would hand every project the packager's paths until the first sync.
    let pointer = crate::workflows::materialize::PROJECT_POINTER;
    if std::fs::symlink_metadata(root.join(pointer)).is_ok() {
        return Err(format!(
            "`{pointer}` is written by the daemon for each checkout; remove it from the bundle"
        ));
    }

    // Materialization reads the block from the bundle directory whether or not `owns:` names it,
    // so here it only has to be there and framed (`materialize::read_block`).
    let mut scanned = Vec::new();
    if let Some(source) = agents_block {
        let framed = crate::ownership::normalise(source).filter(|rel| {
            std::fs::read(root.join(rel))
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|text| crate::workflows::materialize::render_block(&text))
                .is_some()
        });
        let Some(rel) = framed else {
            return Err(format!(
                "agents_block `{source}` must be a UTF-8 file in the bundle whose first line is the \
                 begin marker and last line the end marker of the AI WORKFLOW MANAGED BLOCK"
            ));
        };
        scanned.push(rel);
    }

    let hooks = crate::workflows::hooks::parse(hooks)?;
    crate::workflows::hooks::validate(&hooks, |rel| root.join(rel).is_file())?;

    if let Ok(cockpits) = std::fs::read_dir(root.join(COCKPITS)) {
        let mut names: Vec<String> = cockpits
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("wf-") && name.ends_with(".md"))
            .collect();
        names.sort();
        for name in names {
            require_frontmatter(
                root,
                &format!("{COCKPITS}/{name}"),
                &["name", "description", "tools"],
            )?;
        }
    }
    if let Ok(skills) = std::fs::read_dir(root.join(CLAUDE_SKILLS)) {
        let mut names: Vec<String> = skills
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            require_frontmatter(
                root,
                &format!("{CLAUDE_SKILLS}/{name}/SKILL.md"),
                &["name", "description"],
            )?;
        }
    }

    // Everything that lands in a project, the generated mirror included.
    let files =
        workflows::file_hashes(root).map_err(|error| format!("{}: {error}", root.display()))?;
    scanned.extend(
        files
            .into_keys()
            .filter(|path| crate::workflows::materialize::provided(&entries, path)),
    );
    let mut offending = Vec::new();
    for rel in &scanned {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if names_legacy_root(line) {
                offending.push(format!("{rel}:{}", index + 1));
            }
        }
    }
    if !offending.is_empty() {
        let more = offending.len().saturating_sub(MAX_NAMED);
        offending.truncate(MAX_NAMED);
        let mut said = format!(
            "these lines would be materialized and still point into `.ai/`, which a project using \
             this bundle does not have:\n  {}",
            offending.join("\n  ")
        );
        if more > 0 {
            said.push_str(&format!("\n  …and {more} more"));
        }
        return Err(said);
    }
    Ok(())
}

/// Whether `line` names the legacy `.ai/` root: `.ai/` not glued to a word before it, so
/// `https://x.ai/` or `openai/` are not it, and `` `.ai/` ``, `~/.ai/`, `(.ai/x)` are.
fn names_legacy_root(line: &str) -> bool {
    line.match_indices(".ai/").any(|(at, _)| {
        line[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !(before.is_alphanumeric() || before == '_' || before == '-'))
    })
}

/// Refuse `rel` unless it opens with a YAML frontmatter block naming every key in `keys` with a
/// non-empty value. What a harness reads to list an agent or a skill; one without it is silently
/// not offered, which is the failure this exists to make loud.
fn require_frontmatter(root: &Path, rel: &str, keys: &[&str]) -> Result<(), String> {
    let text =
        std::fs::read_to_string(root.join(rel)).map_err(|error| format!("{rel}: {error}"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    // Fenced on both sides: without the closing `---` the whole document would be read as YAML.
    let lines: Vec<&str> = text.lines().collect();
    let block = (lines.first().map(|line| line.trim_end()) == Some("---"))
        .then(|| lines[1..].iter().position(|line| line.trim_end() == "---"))
        .flatten()
        .map(|end| lines[1..=end].join("\n"));
    let Some(map) = block
        .and_then(|body| serde_yaml::from_str::<serde_yaml::Value>(&body).ok())
        .and_then(|value| value.as_mapping().cloned())
    else {
        return Err(format!(
            "{rel}: no YAML frontmatter (a `---` block on its first line)"
        ));
    };
    for key in keys {
        let present = match map.get(*key) {
            None | Some(serde_yaml::Value::Null) => false,
            Some(serde_yaml::Value::String(value)) => !value.trim().is_empty(),
            Some(serde_yaml::Value::Sequence(values)) => !values.is_empty(),
            Some(_) => true,
        };
        if !present {
            return Err(format!("{rel}: frontmatter has no `{key}`"));
        }
    }
    Ok(())
}

/// Run git in `dir` through the daemon's own runner (`worktree::git`, which disables the
/// repository-named command strings a hostile config could smuggle in).
async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::execution::worktree::git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("git could not be run: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Record `version` as one commit in `home`, initialising the repository the first time.
///
/// Only `<version>/` is staged, so anything else a person keeps under the workflow's folder stays
/// theirs to commit. An identity is supplied only when git has none configured, so a person's own
/// name goes on the history when there is one and a bare machine still gets a history.
async fn commit_version(home: &Path, name: &str, version: &str) -> Result<String, String> {
    if !home.join(".git").exists() {
        git(home, &["init", "--quiet"]).await?;
    }
    git(home, &["add", "--", version]).await?;
    let has_identity = git(home, &["config", "user.email"])
        .await
        .is_ok_and(|email| !email.is_empty());
    let message = format!("{name} {version}");
    let mut args: Vec<&str> = Vec::new();
    if !has_identity {
        args.extend([
            "-c",
            "user.name=NucleOS",
            "-c",
            "user.email=nucleos@localhost",
        ]);
    }
    args.extend(["commit", "--quiet", "-m", &message, "--", version]);
    git(home, &args).await?;
    git(home, &["rev-parse", "HEAD"]).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn project() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        for (path, contents) in [
            (".ai/workflow/workflow.md", "the pipeline"),
            (".ai/workflow/dispatch.md", "dispatch"),
            (".ai/scripts/select_tests.py", "print()"),
            (".ai/scripts/private.py", "not the workflow's"),
            (".ai/memory.md", "this project's own"),
        ] {
            let target = root.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        std::fs::write(
            temp.path().join("bundle.yaml"),
            "# the dev workflow\ndescription: rules\nowns:\n  - .ai/workflow/\n  - .ai/scripts/select_tests.py\n",
        )
        .unwrap();
        temp
    }

    #[tokio::test]
    async fn packaging_copies_what_the_manifest_owns_and_nothing_else() {
        let temp = project();
        let library = temp.path().join("lib");
        let packaged = package(
            &library,
            "dev",
            "1.0.0",
            &temp.path().join("project"),
            &temp.path().join("bundle.yaml"),
        )
        .await
        .unwrap();

        let dir = library.join("dev").join("1.0.0");
        assert_eq!(packaged.dir, dir);
        assert_eq!(packaged.files, 3);
        assert!(dir.join(".ai/workflow/workflow.md").is_file());
        assert!(dir.join(".ai/scripts/select_tests.py").is_file());
        assert!(!dir.join(".ai/scripts/private.py").exists());
        assert!(!dir.join(".ai/memory.md").exists());
        // The manifest as it was written, comments included.
        assert!(
            std::fs::read_to_string(dir.join(workflows::MANIFEST))
                .unwrap()
                .starts_with("# the dev workflow")
        );
        // The library reads it back as a bundle, with the hash a pin would record.
        let bundle = workflows::read_bundle(&dir, "dev", "1.0.0")
            .unwrap()
            .unwrap();
        assert_eq!(bundle.hash, packaged.hash);
        assert_eq!(bundle.owns.len(), 2);

        if git_available() {
            let commit = packaged
                .commit
                .expect("git is here, so the version is committed");
            assert_eq!(commit.len(), 40);
            assert!(library.join("dev").join(".git").exists());
        }
    }

    #[tokio::test]
    async fn a_version_is_written_once() {
        let temp = project();
        let library = temp.path().join("lib");
        let root = temp.path().join("project");
        let manifest = temp.path().join("bundle.yaml");
        package(&library, "dev", "1.0.0", &root, &manifest)
            .await
            .unwrap();
        std::fs::write(root.join(".ai/workflow/workflow.md"), "changed").unwrap();

        let refused = package(&library, "dev", "1.0.0", &root, &manifest)
            .await
            .unwrap_err();
        assert!(refused.contains("already in the library"), "{refused}");
        assert_eq!(
            std::fs::read_to_string(library.join("dev/1.0.0/.ai/workflow/workflow.md")).unwrap(),
            "the pipeline"
        );

        // A second version is a second commit on the same history.
        let second = package(&library, "dev", "1.1.0", &root, &manifest)
            .await
            .unwrap();
        if git_available() {
            let log = git(&library.join("dev"), &["log", "--format=%s"])
                .await
                .unwrap();
            assert_eq!(log.lines().collect::<Vec<_>>(), ["dev 1.1.0", "dev 1.0.0"]);
            assert!(second.commit.is_ok());
        }
    }

    #[tokio::test]
    async fn f5_without_a_usable_history_the_bundle_still_lands_and_says_why() {
        let temp = project();
        let library = temp.path().join("lib");
        let home = library.join("dev");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".git"), "not a git directory").unwrap();

        let packaged = package(
            &library,
            "dev",
            "1.0.0",
            &temp.path().join("project"),
            &temp.path().join("bundle.yaml"),
        )
        .await
        .unwrap();

        assert!(packaged.dir.is_dir());
        assert!(packaged.dir.join(".ai/workflow/workflow.md").is_file());
        assert!(packaged.commit.is_err());
        assert!(packaged.commit.unwrap_err().contains("git"));
    }

    /// The managed block's source ships with the bundle or the bundle is not packaged: without it
    /// a materialization would have no block to write, and nothing to tell a dropped one from.
    #[tokio::test]
    async fn a_managed_block_the_owns_list_does_not_cover_refuses_packaging() {
        let temp = project();
        let library = temp.path().join("lib");
        let root = temp.path().join("project");
        std::fs::write(root.join(".ai/workflow/agents-block.md"), BLOCK).unwrap();
        std::fs::write(root.join("block.md"), BLOCK).unwrap();

        let outside = temp.path().join("outside.yaml");
        std::fs::write(
            &outside,
            "owns:\n  - .ai/workflow/\nagents_block: block.md\n",
        )
        .unwrap();
        let error = package(&library, "dev", "1.0.0", &root, &outside)
            .await
            .unwrap_err();
        assert!(error.contains("block.md"), "{error}");
        assert!(!library.exists());

        let inside = temp.path().join("inside.yaml");
        std::fs::write(
            &inside,
            "owns:\n  - .ai/workflow/\nagents_block: .ai/workflow/agents-block.md\n",
        )
        .unwrap();
        let packaged = package(&library, "dev", "1.0.0", &root, &inside)
            .await
            .unwrap();
        assert!(packaged.dir.join(".ai/workflow/agents-block.md").is_file());
    }

    const BLOCK: &str =
        "# >>> AI WORKFLOW MANAGED BLOCK >>>\n\nrules\n\n# <<< AI WORKFLOW MANAGED BLOCK <<<\n";

    /// The source is the whole region, markers included: one without them would package fine and
    /// then stop every materialization it is pinned into, so it is refused here instead.
    #[tokio::test]
    async fn a_managed_block_source_without_its_markers_refuses_packaging() {
        let temp = project();
        let library = temp.path().join("lib");
        let root = temp.path().join("project");
        std::fs::write(root.join(".ai/workflow/agents-block.md"), "rules\n").unwrap();
        let manifest = temp.path().join("bundle.yaml");
        std::fs::write(
            &manifest,
            "owns:\n  - .ai/workflow/\nagents_block: .ai/workflow/agents-block.md\n",
        )
        .unwrap();
        let error = package(&library, "dev", "1.0.0", &root, &manifest)
            .await
            .unwrap_err();
        assert!(error.contains("MANAGED BLOCK"), "{error}");
        assert!(!library.exists());
    }

    /// A hook script outside `owns:` is still packaged — into the library, where it runs from —
    /// and a hook naming a file the project does not have, or an event nobody fires, refuses the
    /// whole package rather than shipping a guard that would never run.
    #[tokio::test]
    async fn hook_scripts_are_packaged_and_a_hook_that_could_not_run_refuses_packaging() {
        let temp = project();
        let library = temp.path().join("lib");
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join("gates")).unwrap();
        std::fs::write(root.join("gates/stop.py"), "print()").unwrap();

        for bad in [
            "hooks:\n  - event: Stop\n    script: gates/missing.py\n",
            "hooks:\n  - event: Teardown\n    script: gates/stop.py\n",
            "hooks:\n  - event: Stop\n    script: ../gates/stop.py\n",
        ] {
            let manifest = temp.path().join("bad.yaml");
            std::fs::write(&manifest, format!("owns:\n  - .ai/workflow/\n{bad}")).unwrap();
            assert!(
                package(&library, "dev", "1.0.0", &root, &manifest)
                    .await
                    .is_err(),
                "{bad}"
            );
        }
        assert!(!library.exists());

        let manifest = temp.path().join("hooked.yaml");
        std::fs::write(
            &manifest,
            "owns:\n  - .ai/workflow/\nhooks:\n  - event: Stop\n    script: gates/stop.py\n    args: [\"--stop-hook\"]\n",
        )
        .unwrap();
        let packaged = package(&library, "dev", "1.0.0", &root, &manifest)
            .await
            .unwrap();
        assert!(packaged.dir.join("gates/stop.py").is_file());
        assert_eq!(packaged.files, 3);
        let hooks = crate::workflows::hooks::manifest_hooks(&packaged.dir).unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].args, vec!["--stop-hook".to_string()]);
    }

    #[tokio::test]
    async fn a_manifest_naming_a_path_outside_the_project_or_not_in_it_writes_nothing() {
        let temp = project();
        let library = temp.path().join("lib");
        let root = temp.path().join("project");
        for owns in ["../elsewhere", "/etc", ".ai/nope/"] {
            let manifest = temp.path().join("bad.yaml");
            std::fs::write(&manifest, format!("owns:\n  - {owns}\n")).unwrap();
            assert!(
                package(&library, "dev", "1.0.0", &root, &manifest)
                    .await
                    .is_err(),
                "{owns}"
            );
        }
        std::fs::write(temp.path().join("empty.yaml"), "owns: []\n").unwrap();
        assert!(
            package(
                &library,
                "dev",
                "1.0.0",
                &root,
                &temp.path().join("empty.yaml")
            )
            .await
            .is_err()
        );
        assert!(!library.exists());
        assert!(
            package(
                &library,
                "../x",
                "1.0.0",
                &root,
                &temp.path().join("bundle.yaml")
            )
            .await
            .is_err()
        );
    }

    /* ------------------------------------------------------ source trees -- */

    const SOURCE_MANIFEST: &str = "description: dev\nowns:\n  - .nucleos/workflow/\n  \
        - .claude/skills/\n  - .claude/agents/\n  - .agents/skills/\nagents_block: \
        agents-block.md\nhooks:\n  - event: Stop\n    script: scripts/spec_loop.py\n";

    /// A bundle source laid out as `core/assets/nucleos-dev/` is, with `changes` applied: `Some`
    /// writes (or overwrites) a file, `None` leaves one out. Returns (temp, source dir).
    fn source_tree(
        changes: &[(&'static str, Option<&'static str>)],
    ) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("src");
        let mut files: std::collections::BTreeMap<&str, &str> = [
            ("bundle.yaml", SOURCE_MANIFEST),
            (
                "agents-block.md",
                "# >>> AI WORKFLOW MANAGED BLOCK >>>\nrules\n# <<< AI WORKFLOW MANAGED BLOCK <<<\n",
            ),
            (".nucleos/workflow/workflow.md", "# Workflow\nstate lives in <state_dir>\n"),
            (
                ".claude/skills/planner/SKILL.md",
                "---\nname: planner\ndescription: plans\n---\nbody\n",
            ),
            (".claude/skills/planner/ref.md", "more"),
            (
                ".claude/agents/wf-planner.md",
                "---\nname: wf-planner\ndescription: cockpit\ntools: Read, Grep\n---\nbody\n",
            ),
            (".claude/agents/README.md", "not a cockpit, so no frontmatter needed"),
            // Never materialized, so it may still name the old root.
            ("scripts/spec_loop.py", "# migrates the legacy .ai/local/loops/\n"),
            ("scripts/__pycache__/spec_loop.cpython-311.pyc", "bytes"),
        ]
        .into_iter()
        .collect();
        for &(path, contents) in changes {
            match contents {
                Some(contents) => files.insert(path, contents),
                None => files.remove(path),
            };
        }
        for (path, contents) in files {
            let target = root.join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        (temp, root)
    }

    #[tokio::test]
    async fn a_source_tree_is_packaged_whole_with_its_codex_mirror_generated() {
        let (temp, source) = source_tree(&[]);
        let library = temp.path().join("lib");
        let packaged = package_source(&library, "nucleos-dev", "2.0.0", &source)
            .await
            .unwrap();

        let dir = library.join("nucleos-dev").join("2.0.0");
        assert_eq!(packaged.dir, dir);
        // Seven authored files besides the manifest, plus the two of the generated mirror.
        assert_eq!(packaged.files, 9);
        assert!(dir.join("scripts/spec_loop.py").is_file());
        assert!(dir.join("agents-block.md").is_file());
        assert!(!dir.join("scripts/__pycache__").exists());
        for file in ["SKILL.md", "ref.md"] {
            assert_eq!(
                std::fs::read(dir.join(".agents/skills/planner").join(file)).unwrap(),
                std::fs::read(source.join(".claude/skills/planner").join(file)).unwrap(),
            );
        }
        assert!(!source.join(".agents").exists(), "the source is never written to");
        assert!(!library.join("nucleos-dev").join(".2.0.0.staging").exists());

        let bundle = workflows::read_bundle(&dir, "nucleos-dev", "2.0.0")
            .unwrap()
            .unwrap();
        assert_eq!(bundle.hash, packaged.hash);
        assert_eq!(bundle.agents_block.as_deref(), Some("agents-block.md"));
        assert_eq!(
            crate::workflows::hooks::manifest_hooks(&dir).unwrap()[0].script,
            "scripts/spec_loop.py"
        );

        // Written once, from a source as from a project.
        assert!(
            package_source(&library, "nucleos-dev", "2.0.0", &source)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_source_tree_that_fails_a_check_is_refused_and_leaves_nothing() {
        for (changes, expected) in [
            (
                vec![(
                    ".claude/agents/wf-planner.md",
                    Some("---\nname: wf-planner\ndescription: cockpit\n---\nbody\n"),
                )],
                ".claude/agents/wf-planner.md: frontmatter has no `tools`",
            ),
            (
                vec![(".claude/skills/planner/SKILL.md", Some("# planner\nno frontmatter\n"))],
                ".claude/skills/planner/SKILL.md: no YAML frontmatter",
            ),
            (
                vec![(
                    ".claude/skills/planner/SKILL.md",
                    Some("---\nname: planner\ndescription: plans\nbody, never closed\n"),
                )],
                "no YAML frontmatter",
            ),
            (
                vec![(
                    ".nucleos/workflow/workflow.md",
                    Some("# Workflow\nread `.ai/memory.md` first\n"),
                )],
                ".nucleos/workflow/workflow.md:2",
            ),
            (
                vec![(
                    ".claude/skills/planner/ref.md",
                    Some("one\ntwo\nsee (.ai/plans/x.md)\n"),
                )],
                ".agents/skills/planner/ref.md:3",
            ),
            (
                vec![(
                    "agents-block.md",
                    Some(
                        "# >>> AI WORKFLOW MANAGED BLOCK >>>\nsee .ai/workflow\n\
                         # <<< AI WORKFLOW MANAGED BLOCK <<<\n",
                    ),
                )],
                "agents-block.md:2",
            ),
            (
                vec![("agents-block.md", Some("rules, unframed\n"))],
                "agents_block `agents-block.md`",
            ),
            (
                vec![(".nucleos/workflow/workflow.md", None)],
                "`.nucleos/workflow` is under `owns:` and not in the bundle",
            ),
            (
                vec![("scripts/spec_loop.py", None)],
                "hook script `scripts/spec_loop.py` is not a file in the bundle",
            ),
            (
                vec![(".agents/skills/planner/SKILL.md", Some("authored by hand"))],
                "is generated from .claude/skills/",
            ),
            (vec![("bundle.yaml", Some("owns: .nucleos/\n"))], "bundle.yaml"),
            (
                vec![(".nucleos/workflow/project.json", Some("{}"))],
                "is written by the daemon for each checkout",
            ),
        ] {
            let (temp, source) = source_tree(&changes);
            let library = temp.path().join("lib");
            let error = package_source(&library, "nucleos-dev", "2.0.0", &source)
                .await
                .unwrap_err();
            assert!(error.contains(expected), "{expected}\n---\n{error}");
            let home = library.join("nucleos-dev");
            assert!(!home.join("2.0.0").exists(), "{expected}");
            assert!(!home.join(".2.0.0.staging").exists(), "{expected}");
        }
    }

    #[test]
    fn only_a_free_standing_dot_ai_names_the_legacy_root() {
        for line in [".ai/x", "see `.ai/memory.md`", "~/.ai/local", "(.ai/plans)", "a .ai/b"] {
            assert!(names_legacy_root(line), "{line}");
        }
        for line in [
            "https://x.ai/docs",
            "openai/gpt",
            "my-tool.ai/x",
            ".nucleos/workflow",
            "ai/",
        ] {
            assert!(!names_legacy_root(line), "{line}");
        }
    }
}
