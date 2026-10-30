//! Where a document applies: the machine, or one project. Stored as text in `settings.scope`.

use std::fmt;
use std::str::FromStr;

/// The two families of scope. A document belongs to exactly one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Machine,
    Project,
}

/// `machine` or `project:<id>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    Machine,
    Project(String),
}

impl Scope {
    pub fn project(id: impl Into<String>) -> Self {
        Self::Project(id.into())
    }

    pub fn kind(&self) -> Kind {
        todo!("RED stub: Scope::kind")
    }

    /// The project id, for the feed line and for anything keyed on a project.
    pub fn project_id(&self) -> Option<&str> {
        todo!("RED stub: Scope::project_id")
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("RED stub: Scope Display")
    }
}

/// The text that is not a scope, verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeParseError(pub String);

impl fmt::Display for ScopeParseError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        todo!("RED stub: ScopeParseError Display")
    }
}

impl std::error::Error for ScopeParseError {}

impl FromStr for Scope {
    type Err = ScopeParseError;

    fn from_str(_text: &str) -> Result<Self, Self::Err> {
        todo!("RED stub: Scope FromStr")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_prints_and_parses_back() {
        assert_eq!(Scope::Machine.to_string(), "machine");
        assert_eq!(Scope::project("p1").to_string(), "project:p1");
        assert_eq!("machine".parse::<Scope>(), Ok(Scope::Machine));
        assert_eq!("project:p1".parse::<Scope>(), Ok(Scope::project("p1")));
    }

    #[test]
    fn a_project_id_may_contain_a_colon() {
        let scope: Scope = "project:org:repo".parse().unwrap();
        assert_eq!(scope.project_id(), Some("org:repo"));
        assert_eq!(scope.to_string(), "project:org:repo");
    }

    #[test]
    fn anything_else_is_refused() {
        for text in ["", "Machine", "projects:x", "project:", "project:a b", "project", " machine", "machine "] {
            let error = text.parse::<Scope>().expect_err(text);
            assert_eq!(error, ScopeParseError(text.to_owned()));
            assert!(error.to_string().contains("machine"), "the message names the valid forms");
        }
    }

    #[test]
    fn kind_and_project_id_follow_the_variant() {
        assert_eq!(Scope::Machine.kind(), Kind::Machine);
        assert_eq!(Scope::Machine.project_id(), None);
        assert_eq!(Scope::project("p1").kind(), Kind::Project);
        assert_eq!(Scope::project("p1").project_id(), Some("p1"));
    }

    #[test]
    fn scopes_order_machine_first() {
        let mut scopes = vec![Scope::project("b"), Scope::Machine, Scope::project("a")];
        scopes.sort();
        assert_eq!(scopes, vec![Scope::Machine, Scope::project("a"), Scope::project("b")]);
    }
}
