//! §spec agentes-agnosticos
//!
//! The format of an agent package and the code that reads one: a directory
//! `<name>/<version>/` holding `agent.md` (YAML frontmatter, then the prompt body), `output.md`
//! (the shape of what the agent hands back) and whatever `knowledge/` files the frontmatter names.
//!
//! # Parsing is pure and strict
//!
//! [`parse_package`] touches nothing but the directory it is given and refuses anything the spec
//! (section 3) does not describe: an unknown frontmatter key, an unknown capability or context
//! source, a name or version that disagrees with the frontmatter or with the directory it sits in,
//! a knowledge path that leaves the package, an `inject` file that is not there, a missing
//! `output.md`. A package that parses is one the launcher can use without checking again.
//!
//! # A package is addressed by `name@X.Y.Z`
//!
//! [`PackageRef`] is that address. A reference without a version is refused rather than resolved to
//! the highest one: which version an agent runs is a decision the catalog row records, not
//! something a lookup may guess. [`highest_version`] exists for the installer, which does have to
//! pick the row that wins.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use nucleos_base::agent::Capability;

/// The file inside an installed package that records the hash of what was installed.
pub const HASH_FILE: &str = "content.sha256";

/// Why a package could not be read.
#[derive(Debug)]
pub enum PackageError {
    /// A reference had no `@version`.
    MissingVersion(String),
    /// A package name is not `[a-z][a-z0-9-]*`.
    BadName(String),
    /// A version is not strictly `X.Y.Z`.
    BadVersion(String),
    /// A file could not be read.
    Io(String),
    /// `agent.md` has no frontmatter, or it does not parse.
    Frontmatter(String),
    /// The frontmatter's `name` is not the one expected.
    NameMismatch { expected: String, found: String },
    /// The frontmatter's `version` is not the one expected.
    VersionMismatch { expected: String, found: String },
    /// The directory the package sits in is not `<name>/<version>`.
    DirMismatch(String),
    /// A capability the frontmatter lists is not one the system knows.
    UnknownCapability(String),
    /// A context source the frontmatter lists is not one the system knows.
    UnknownContext(String),
    /// `knowledge.on_demand` is set, but the agent cannot read files to fetch it.
    OnDemandNeedsFsRead,
    /// A knowledge path is absolute or climbs out of the package.
    PathEscapes(String),
    /// A file the package needs is not there.
    MissingFile(String),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVersion(reference) => write!(
                f,
                "`{reference}` names no version; a package is referenced as `name@X.Y.Z`"
            ),
            Self::BadName(name) => write!(
                f,
                "`{name}` is not a usable package name (lowercase letters, digits and `-`, \
                 starting with a letter)"
            ),
            Self::BadVersion(version) => {
                write!(f, "`{version}` is not a version (expected `X.Y.Z`)")
            }
            Self::Io(message) => write!(f, "{message}"),
            Self::Frontmatter(message) => write!(f, "agent.md frontmatter: {message}"),
            Self::NameMismatch { expected, found } => write!(
                f,
                "agent.md says the package is named `{found}`, expected `{expected}`"
            ),
            Self::VersionMismatch { expected, found } => write!(
                f,
                "agent.md says the package is version `{found}`, expected `{expected}`"
            ),
            Self::DirMismatch(message) => write!(f, "{message}"),
            Self::UnknownCapability(capability) => {
                write!(f, "`{capability}` is not a known capability")
            }
            Self::UnknownContext(source) => write!(
                f,
                "`{source}` is not a known context source (project.instructions, \
                 project.memory, project.decisions)"
            ),
            Self::OnDemandNeedsFsRead => write!(
                f,
                "knowledge.on_demand needs the `fs.read` capability to be fetched"
            ),
            Self::PathEscapes(path) => write!(
                f,
                "`{path}` must be a relative path inside the package (no `..`, nothing absolute)"
            ),
            Self::MissingFile(path) => write!(f, "`{path}` is missing from the package"),
        }
    }
}

impl std::error::Error for PackageError {}

/// `name@X.Y.Z`: one version of one package.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PackageRef {
    pub name: String,
    pub version: String,
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `X.Y.Z` with every part a non-empty run of digits that fits a `u64`.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let mut next = || {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<u64>().ok()
    };
    let key = (next()?, next()?, next()?);
    parts.next().is_none().then_some(key)
}

impl PackageRef {
    /// Parse `name@X.Y.Z`. A reference with no `@` is [`PackageError::MissingVersion`].
    pub fn parse(text: &str) -> Result<Self, PackageError> {
        let Some((name, version)) = text.split_once('@') else {
            return Err(PackageError::MissingVersion(text.to_string()));
        };
        if !valid_name(name) {
            return Err(PackageError::BadName(name.to_string()));
        }
        if parse_version(version).is_none() {
            return Err(PackageError::BadVersion(version.to_string()));
        }
        Ok(Self {
            name: name.to_string(),
            version: version.to_string(),
        })
    }

    /// The version as numbers, so `1.10.0` sorts above `1.9.0`.
    pub fn version_key(&self) -> (u64, u64, u64) {
        parse_version(&self.version).unwrap_or((0, 0, 0))
    }
}

impl fmt::Display for PackageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// A piece of the project an agent is given as context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContextSource {
    ProjectInstructions,
    ProjectMemory,
    ProjectDecisions,
}

impl ContextSource {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "project.instructions" => Some(Self::ProjectInstructions),
            "project.memory" => Some(Self::ProjectMemory),
            "project.decisions" => Some(Self::ProjectDecisions),
            _ => None,
        }
    }
}

/// A package, parsed and validated.
#[derive(Clone, Debug)]
pub struct Package {
    pub reference: PackageRef,
    pub description: String,
    pub capabilities: Vec<Capability>,
    pub context: Vec<ContextSource>,
    /// Paths relative to [`Package::root`], read into the prompt.
    pub inject: Vec<String>,
    /// Globs relative to [`Package::root`], left for the agent to fetch.
    pub on_demand: Vec<String>,
    /// The part of `agent.md` after the frontmatter.
    pub body: String,
    pub output_md: String,
    /// The directory the package was read from.
    pub root: PathBuf,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    name: String,
    version: String,
    description: String,
    capabilities: Vec<String>,
    #[serde(default)]
    context: Vec<String>,
    #[serde(default)]
    knowledge: Knowledge,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Knowledge {
    #[serde(default)]
    inject: Vec<String>,
    #[serde(default)]
    on_demand: Vec<String>,
}

/// Split `agent.md` into its YAML frontmatter and its body.
///
/// The file must start with a `---` line; the frontmatter runs to the next `---` line, and the body
/// is the rest, minus one leading newline.
fn split_frontmatter(text: &str) -> Result<(&str, &str), PackageError> {
    let missing = || PackageError::Frontmatter("the file must start with a `---` line".into());
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().ok_or_else(missing)?;
    if first.trim_end() != "---" {
        return Err(missing());
    }
    let start = first.len();
    let mut offset = start;
    for line in lines {
        if line.trim_end() == "---" {
            let body = &text[offset + line.len()..];
            let body = body
                .strip_prefix("\r\n")
                .or_else(|| body.strip_prefix('\n'))
                .unwrap_or(body);
            return Ok((&text[start..offset], body));
        }
        offset += line.len();
    }
    Err(PackageError::Frontmatter(
        "the frontmatter is not closed by a second `---` line".into(),
    ))
}

/// Whether a knowledge path stays inside the package it is read from.
fn stays_inside(path: &str) -> bool {
    if path.is_empty() || path.contains('\\') {
        return false;
    }
    Path::new(path)
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

fn read(path: &Path, shown: &str) -> Result<String, PackageError> {
    if !path.is_file() {
        return Err(PackageError::MissingFile(shown.to_string()));
    }
    std::fs::read_to_string(path)
        .map_err(|error| PackageError::Io(format!("{}: {error}", path.display())))
}

/// Read and validate the package in `dir`, which must be `<name>/<version>` for `expected`.
pub fn parse_package(dir: &Path, expected: &PackageRef) -> Result<Package, PackageError> {
    let version_dir = dir.file_name().and_then(|n| n.to_str());
    let name_dir = dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());
    if version_dir != Some(expected.version.as_str()) || name_dir != Some(expected.name.as_str()) {
        return Err(PackageError::DirMismatch(format!(
            "{} is not the directory of `{expected}` (expected `{}/{}`)",
            dir.display(),
            expected.name,
            expected.version
        )));
    }

    let text = read(&dir.join("agent.md"), "agent.md")?;
    let (yaml, body) = split_frontmatter(&text)?;
    let front: Frontmatter =
        serde_yaml::from_str(yaml).map_err(|error| PackageError::Frontmatter(error.to_string()))?;

    if front.name != expected.name {
        return Err(PackageError::NameMismatch {
            expected: expected.name.clone(),
            found: front.name,
        });
    }
    if front.version != expected.version {
        return Err(PackageError::VersionMismatch {
            expected: expected.version.clone(),
            found: front.version,
        });
    }

    let mut capabilities = Vec::new();
    for text in &front.capabilities {
        let capability =
            Capability::parse(text).ok_or_else(|| PackageError::UnknownCapability(text.clone()))?;
        if !capabilities.contains(&capability) {
            capabilities.push(capability);
        }
    }

    let mut context = Vec::new();
    for text in &front.context {
        let source =
            ContextSource::parse(text).ok_or_else(|| PackageError::UnknownContext(text.clone()))?;
        if !context.contains(&source) {
            context.push(source);
        }
    }

    let Knowledge { inject, on_demand } = front.knowledge;
    if !on_demand.is_empty() && !capabilities.contains(&Capability::FsRead) {
        return Err(PackageError::OnDemandNeedsFsRead);
    }
    for path in inject.iter().chain(&on_demand) {
        if !stays_inside(path) {
            return Err(PackageError::PathEscapes(path.clone()));
        }
    }
    for path in &inject {
        if !dir.join(path).is_file() {
            return Err(PackageError::MissingFile(path.clone()));
        }
    }

    let output_md = read(&dir.join("output.md"), "output.md")?;

    Ok(Package {
        reference: expected.clone(),
        description: front.description,
        capabilities,
        context,
        inject,
        on_demand,
        body: body.to_string(),
        output_md,
        root: dir.to_path_buf(),
    })
}

/// Where `reference` is installed under `agents_home`: `<home>/<name>/<version>`.
pub fn installed_root(agents_home: &Path, reference: &PackageRef) -> PathBuf {
    agents_home.join(&reference.name).join(&reference.version)
}

/// Read an installed package back.
pub fn load_installed(agents_home: &Path, reference: &PackageRef) -> Result<Package, PackageError> {
    parse_package(&installed_root(agents_home, reference), reference)
}

/// The highest installed version of `name`, ignoring directories that are not valid versions.
pub fn highest_version(agents_home: &Path, name: &str) -> Option<PackageRef> {
    let entries = std::fs::read_dir(agents_home.join(name)).ok()?;
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let version = entry.file_name().into_string().ok()?;
            PackageRef::parse(&format!("{name}@{version}")).ok()
        })
        .max_by_key(PackageRef::version_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "---\n\
        name: demo\n\
        version: 1.0.0\n\
        description: A demo.\n\
        capabilities: [fs.read]\n\
        context: [project.instructions]\n\
        knowledge:\n  inject: [knowledge/a.md]\n  on_demand: [knowledge/guides/*.md]\n\
        ---\n\
        Body line.\n";

    /// Write a package whose `agent.md` is `agent`, returning its directory.
    fn package_dir(root: &Path, agent: &str) -> PathBuf {
        let dir = root.join("demo").join("1.0.0");
        std::fs::create_dir_all(dir.join("knowledge")).unwrap();
        std::fs::write(dir.join("agent.md"), agent).unwrap();
        std::fs::write(dir.join("output.md"), "Report.\n").unwrap();
        std::fs::write(dir.join("knowledge/a.md"), "A\n").unwrap();
        dir
    }

    fn demo() -> PackageRef {
        PackageRef::parse("demo@1.0.0").unwrap()
    }

    fn with_front(front: &str) -> String {
        format!("---\n{front}---\nBody\n")
    }

    const BASE: &str = "name: demo\nversion: 1.0.0\ndescription: d\ncapabilities: [fs.read]\n";

    #[test]
    fn a_valid_package_parses() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package_dir(tmp.path(), AGENT);
        let pkg = parse_package(&dir, &demo()).unwrap();
        assert_eq!(pkg.reference, demo());
        assert_eq!(pkg.description, "A demo.");
        assert_eq!(pkg.capabilities, vec![Capability::FsRead]);
        assert_eq!(pkg.context, vec![ContextSource::ProjectInstructions]);
        assert_eq!(pkg.inject, vec!["knowledge/a.md"]);
        assert_eq!(pkg.on_demand, vec!["knowledge/guides/*.md"]);
        assert_eq!(pkg.body, "Body line.\n");
        assert_eq!(pkg.output_md, "Report.\n");
        assert_eq!(pkg.root, dir);
    }

    #[test]
    fn invalid_frontmatter_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        for text in [
            "no frontmatter\n",
            "---\nname: demo\n",
            "---\n: : [\n---\nb\n",
        ] {
            let dir = package_dir(tmp.path(), text);
            assert!(
                matches!(
                    parse_package(&dir, &demo()),
                    Err(PackageError::Frontmatter(_))
                ),
                "{text:?}"
            );
        }
    }

    #[test]
    fn an_unknown_field_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package_dir(tmp.path(), &with_front(&format!("{BASE}colour: red\n")));
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::Frontmatter(_))
        ));
        let nested = format!("{BASE}knowledge:\n  extra: [x]\n");
        let dir = package_dir(tmp.path(), &with_front(&nested));
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::Frontmatter(_))
        ));
    }

    #[test]
    fn an_unknown_context_or_capability_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package_dir(
            tmp.path(),
            &with_front(&format!("{BASE}context: [project.secrets]\n")),
        );
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::UnknownContext(_))
        ));
        let bad = BASE.replace("fs.read", "teleport");
        let dir = package_dir(tmp.path(), &with_front(&bad));
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::UnknownCapability(_))
        ));
    }

    #[test]
    fn on_demand_needs_fs_read() {
        let tmp = tempfile::tempdir().unwrap();
        let front = "name: demo\nversion: 1.0.0\ndescription: d\ncapabilities: []\n\
                     knowledge:\n  on_demand: [knowledge/*.md]\n";
        let dir = package_dir(tmp.path(), &with_front(front));
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::OnDemandNeedsFsRead)
        ));
    }

    #[test]
    fn a_path_that_leaves_the_package_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        for path in ["../x.md", "knowledge/../../x.md", "/etc/passwd", "a\\..\\b"] {
            let front = format!("{BASE}knowledge:\n  inject: ['{path}']\n");
            let dir = package_dir(tmp.path(), &with_front(&front));
            assert!(
                matches!(
                    parse_package(&dir, &demo()),
                    Err(PackageError::PathEscapes(_))
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn a_missing_inject_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let front = format!("{BASE}knowledge:\n  inject: [knowledge/none.md]\n");
        let dir = package_dir(tmp.path(), &with_front(&front));
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::MissingFile(_))
        ));
    }

    #[test]
    fn the_directory_must_match_name_and_version() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package_dir(tmp.path(), AGENT);
        let other = PackageRef::parse("demo@2.0.0").unwrap();
        assert!(matches!(
            parse_package(&dir, &other),
            Err(PackageError::DirMismatch(_))
        ));
        let other = PackageRef::parse("other@1.0.0").unwrap();
        assert!(matches!(
            parse_package(&dir, &other),
            Err(PackageError::DirMismatch(_))
        ));
        // The frontmatter disagreeing with an otherwise right directory.
        let dir = package_dir(tmp.path(), &AGENT.replace("name: demo", "name: renamed"));
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::NameMismatch { .. })
        ));
        let dir = package_dir(
            tmp.path(),
            &AGENT.replace("version: 1.0.0", "version: 1.0.1"),
        );
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn a_missing_output_md_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package_dir(tmp.path(), AGENT);
        std::fs::remove_file(dir.join("output.md")).unwrap();
        assert!(matches!(
            parse_package(&dir, &demo()),
            Err(PackageError::MissingFile(_))
        ));
    }

    #[test]
    fn a_reference_without_a_version_is_refused() {
        assert!(matches!(
            PackageRef::parse("demo"),
            Err(PackageError::MissingVersion(_))
        ));
        assert!(matches!(
            PackageRef::parse("Demo@1.0.0"),
            Err(PackageError::BadName(_))
        ));
        for bad in [
            "demo@1.0",
            "demo@1.0.0.0",
            "demo@v1.0.0",
            "demo@1..0",
            "demo@",
        ] {
            assert!(
                matches!(PackageRef::parse(bad), Err(PackageError::BadVersion(_))),
                "{bad}"
            );
        }
        let parsed = PackageRef::parse("demo-two@1.10.3").unwrap();
        assert_eq!(parsed.to_string(), "demo-two@1.10.3");
        assert_eq!(parsed.version_key(), (1, 10, 3));
    }

    #[test]
    fn highest_version_is_numeric_and_ignores_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        for version in ["1.9.0", "1.10.0", "1.2.0", "latest", "1.0", ".staging"] {
            std::fs::create_dir_all(home.join("demo").join(version)).unwrap();
        }
        // A file named like a higher version is not a package either.
        std::fs::write(home.join("demo").join("9.9.9"), "x").unwrap();
        assert_eq!(
            highest_version(home, "demo"),
            Some(PackageRef::parse("demo@1.10.0").unwrap())
        );
        assert_eq!(highest_version(home, "absent"), None);
    }

    #[test]
    fn an_installed_package_loads_back() {
        let tmp = tempfile::tempdir().unwrap();
        package_dir(tmp.path(), AGENT);
        let reference = demo();
        assert_eq!(
            installed_root(tmp.path(), &reference),
            tmp.path().join("demo").join("1.0.0")
        );
        let pkg = load_installed(tmp.path(), &reference).unwrap();
        assert_eq!(pkg.reference, reference);
    }
}
