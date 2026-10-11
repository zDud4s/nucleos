//! Validated GitHub identifier types and project repository discovery.

use super::*;

/// A repository, as `owner/name`.
///
/// Its own type because every route into this module names one, and because `owner/name` is a shape
/// a plain string does not carry: told `--repo=name` the CLI would fall back to the checkout's own
/// remote, which is a different repository than the one the caller asked about and no error anywhere
/// would say so.
///
/// **An argv guard and a shape check, not an existence check.** A repository nobody has ever
/// created passes here; GitHub is the authority on which ones exist, and a request for one that
/// does not is a failed call carrying the CLI's own message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Repo(String);

impl Repo {
    pub fn new(value: &str) -> Result<Self, String> {
        let value = argv_safe(value, "repository")?;
        match value.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Self(value))
            }
            _ => Err(format!("a repository must be spelled owner/name: {value}")),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// PURE: the repository a git remote URL names, when the remote is one of GitHub's.
    ///
    /// **The single spelling of this translation, and that is the whole reason it is a function.**
    /// `GET /projects/detect` already reads a folder's `origin` and reports the URL verbatim; this
    /// is the step after it, and a second copy of it somewhere else is how the wizard and this route
    /// would come to disagree about which repository a project is — a disagreement neither side
    /// could see, because both would be reporting a plausible `owner/name`.
    ///
    /// `None` for every remote this cannot read, and the caller must not flatten that into an
    /// error: a project on GitLab, a remote that is a folder on a disk, and a `github.com` URL that
    /// names no repository are all ordinary things for a project to have. What they have in common
    /// is that `gh` has nothing to be pointed at, which is the only fact this answers.
    ///
    /// **`github.com` and nothing else.** An Enterprise host is a real deployment and it is not this
    /// one: `gh` here is the daemon's, configured for the public host, so admitting
    /// `github.example.com` would produce a repository the CLI would then fail to reach with a
    /// message about a host nobody named. Narrow and said out loud beats wide and wrong.
    pub fn from_remote_url(url: &str) -> Option<Self> {
        const GITHUB_HOST: &str = "github.com";

        let url = url.trim();
        let (authority, path) = match url.split_once("://") {
            // `https://github.com/owner/name.git`, `ssh://git@github.com/owner/name`.
            Some((_, rest)) => rest.split_once('/')?,
            // The scp-like spelling git writes for `git@github.com:owner/name.git`: no scheme, and
            // the path hangs off a colon rather than a slash. Split before any `@` is stripped, so
            // that a path containing one cannot be mistaken for userinfo.
            None => url.split_once(':')?,
        };
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = host.split_once(':').map_or(host, |(host, _)| host);
        if !host.eq_ignore_ascii_case(GITHUB_HOST) {
            return None;
        }

        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        // Still `Repo::new` that decides, exactly as everywhere else in this module: this function
        // finds the candidate, and the type is what admits it. A remote whose path is `owner` or
        // `owner/name/extra` is refused here for the same reason a caller naming one would be.
        Repo::new(path).ok()
    }
}

/// Which repository a registered project's `origin` points at, or why there is no answer.
///
/// **Every arm but the first is an ordinary state and none of them is an error**, which is why this
/// is a value and not a `Result`. A typed read carries a `Repo`, and until this landed nothing in
/// the daemon could produce one for a project — so a page asking "what is happening on this
/// project's remote" had to either guess or say nothing. The arms are separate because the sentence
/// a person needs is different in each: a project switched `off` has had its root cleared and wants
/// a mode change, a root that is gone wants a folder, a repository with no `origin` wants a remote
/// added, and a project on GitLab wants nothing at all — it is simply not a question this app
/// answers, and saying so is the honest end of it.
///
/// It is `map_orphan::Orphan`'s discipline applied to a smaller question: *I looked and there is
/// none* and *I could not look* must never arrive as the same word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectRepo {
    /// `origin` is a GitHub repository, and this is it.
    ///
    /// Carries the URL as well as the slug because they are two different facts and the page shows
    /// both: the slug is what a read is sent, and the URL is what somebody checks it against when
    /// the slug is not the repository they expected.
    Known { repo: Repo, remote: String },
    /// The project is on the roster with no folder named.
    ///
    /// The state a project in `off` mode is in — `set_project_mode` clears the root — so this is not
    /// a broken row, it is the ordinary shape of a project nobody is running.
    NoRoot,
    /// A folder is named and it is not there.
    ///
    /// Distinct from [`ProjectRepo::NoRoot`] for the reason `map_orphan` keeps `Unreadable` apart
    /// from `NoRepository`: the advice differs completely. One asks somebody to point the project
    /// somewhere; the other says the place it points has moved or been deleted.
    RootMissing { root: String },
    /// The folder is there and git does not keep anything in it.
    ///
    /// `set_project_mode` only insists on a repository for `active`, so a registered project that is
    /// not a checkout is a state this daemon deliberately allows.
    NotARepository { root: String },
    /// A repository with no `origin`.
    ///
    /// A local-only project is a project — `origin_and_head` says so about the folder a wizard is
    /// looking at, and it stays true of one already on the roster.
    NoRemote { root: String },
    /// There is an `origin` and it is not a GitHub repository.
    ///
    /// Carries the URL, because the whole content of this answer is *this is where your project
    /// points and it is not somewhere `gh` can be sent*, and that sentence is unreadable without it.
    NotGithub { remote: String },
}

/// Which repository a project's `origin` points at.
///
/// **The one production path from a project id to a `Repo`.** `vcs::resolve_repo` answers the
/// neighbouring question — which lock on this machine a project shares — and returns a folder; this
/// answers which repository on GitHub it is, and the two must not be confused for each other.
///
/// The remote is read with [`crate::execution::git_exec::origin_and_head`], which is the reader
/// `GET /projects/detect` already uses. Its `head` is thrown away here and that is deliberate: one
/// reader of `origin` in this daemon rather than two, and the cost of the second local `git log` is
/// smaller than the cost of the two readers eventually disagreeing.
///
/// `Ok(None)` is a project the roster has never heard of, and `Err` is the database failing.
/// Everything else the filesystem or git can say is an arm of [`ProjectRepo`].
pub async fn project_repo(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    deadline: std::time::Instant,
) -> Result<Option<ProjectRepo>, sqlx::Error> {
    // Asked separately from the root, because `project_root` cannot tell a project that has no
    // folder from one the roster does not hold — both are `None` — and those are a 200 and a 404.
    let known: Option<String> =
        sqlx::query_scalar("SELECT project_id FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    if known.is_none() {
        return Ok(None);
    }

    let Some(root) = crate::inspect::project_root(pool, project_id).await? else {
        return Ok(Some(ProjectRepo::NoRoot));
    };

    let path = std::path::PathBuf::from(&root);
    if !path.is_dir() {
        return Ok(Some(ProjectRepo::RootMissing { root }));
    }
    // `.exists()` and not `.is_dir()`: in a linked worktree `.git` is a FILE pointing at the real
    // one, and a check that insisted on a directory would report every worktree as not a
    // repository. `detect::inspect_folder` decides `is_git` the same way.
    if !path.join(".git").exists() {
        return Ok(Some(ProjectRepo::NotARepository { root }));
    }

    let (remote, _head) = crate::execution::git_exec::origin_and_head(&path, deadline).await;
    let Some(remote) = remote else {
        return Ok(Some(ProjectRepo::NoRemote { root }));
    };

    Ok(Some(match Repo::from_remote_url(&remote) {
        Some(repo) => ProjectRepo::Known { repo, remote },
        None => ProjectRepo::NotGithub { remote },
    }))
}

/// A branch name this module is willing to put on a `gh` command line.
///
/// Its own type rather than `vcs::Branch`, and deliberately so: that one is a guard for git's argv
/// and this one is a guard for `gh`'s. They happen to check the same three properties today and
/// nothing says they must stay that way — sharing the type would make "the queue accepts this
/// branch" and "GitHub accepts this branch" literally the same sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Branch(String);

impl Branch {
    pub fn new(value: &str) -> Result<Self, String> {
        argv_safe(value, "branch name").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A workflow, named as `gh workflow run` accepts one: a display name, a file name, or an id.
///
/// Whitespace is allowed, because a workflow called `CI Build` is an ordinary thing and this value
/// is one argv element rather than a line to be split. A leading `-` is refused anyway, and that
/// costs the theoretical workflow named `-x`. It is refused on purpose: the guarantee that no caller
/// value can act as an option should not depend on a `--` staying where somebody put it, which is
/// exactly the way `vcs.rs` watched a comment become wrong without anybody editing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowName(String);

impl WorkflowName {
    pub fn new(value: &str) -> Result<Self, String> {
        let value = single_line(value, "workflow name", 255)?;
        if value.starts_with('-') {
            return Err(format!("a workflow name may not start with '-': {value}"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A pull request title. GitHub's own ceiling is 256 characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Title(String);

impl Title {
    pub fn new(value: &str) -> Result<Self, String> {
        single_line(value, "title", 256).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The body of a pull request or a comment: many lines, and the only free text this module puts on
/// a command line.
///
/// Newlines and tabs pass and every other control character does not. It reaches argv only inside
/// `--body=<value>`, so its content cannot become an option however it is spelled. The ceiling is
/// GitHub's own for issue and pull-request bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Body(String);

impl Body {
    pub fn new(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.is_empty() {
            return Err("a body may not be empty".to_owned());
        }
        if value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err("a body may not contain control characters".to_owned());
        }
        if value.chars().count() > 65_536 {
            return Err("a body may not be longer than 65536 characters".to_owned());
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A workflow run's id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunId(String);

impl RunId {
    pub fn new(value: &str) -> Result<Self, String> {
        numeric(value, "run id").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A pull request's number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrNumber(String);

impl PrNumber {
    pub fn new(value: &str) -> Result<Self, String> {
        numeric(value, "pull request number").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An issue's number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssueNumber(String);

impl IssueNumber {
    pub fn new(value: &str) -> Result<Self, String> {
        numeric(value, "issue number").map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
