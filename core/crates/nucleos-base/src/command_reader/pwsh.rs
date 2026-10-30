//! PowerShell command-name reader: a closed-subset scanner first, a resident `pwsh` parser as the
//! second engine. Built and tested on its own; nothing here is wired into `classify`/`judge` yet.
//!
//! "Command name" = the text of `CommandElements[0]` of every `CommandAst`, document order,
//! nested ones included. Whatever the scanner does not recognise is an `Err` (fail closed).

pub mod engine;
mod scanner;
mod scanner_expr;
mod scanner_lex;
mod scanner_stmt;
#[cfg(test)]
mod scanner_tests_common;
#[cfg(test)]
mod scanner_tests_s1;
#[cfg(test)]
mod scanner_tests_s2;
#[cfg(test)]
mod scanner_tests_s3;
#[cfg(test)]
mod scanner_tests_s4;
#[cfg(test)]
mod scanner_tests_s5;
#[cfg(test)]
mod scanner_tests_s6;
#[cfg(test)]
mod scanner_tests_s7;
#[cfg(test)]
mod scanner_tests_s8;
#[cfg(test)]
mod scanner_tests_s9;

/// First word of every simple command, document order, nested included. Err = unreadable (fail closed).
pub fn command_names(src: &str) -> Result<Vec<String>, String> {
    scanner::command_names(src)
}

/// Policy: scanner Ok -> use it; scanner Err -> engine; engine Err/down -> Unreadable.
pub fn read_lines(src: &str, engine: Option<&mut engine::ResidentParser>) -> PwshReading {
    read_lines_with(src, engine)
}

/// A second engine that can list command names; implemented by `ResidentParser` and by test fakes.
pub(crate) trait NamesEngine {
    fn names(&mut self, src: &str) -> Result<Vec<String>, engine::EngineError>;
}

impl NamesEngine for engine::ResidentParser {
    fn names(&mut self, src: &str) -> Result<Vec<String>, engine::EngineError> {
        self.command_names(src)
    }
}

/// Generic form of `read_lines`: scanner first, then the engine.
pub(crate) fn read_lines_with<E: NamesEngine>(src: &str, engine: Option<&mut E>) -> PwshReading {
    let scanner_err = match command_names(src) {
        Ok(names) => {
            return PwshReading::Names {
                names,
                source: Source::Scanner,
            };
        }
        Err(e) => e,
    };
    let Some(engine) = engine else {
        return PwshReading::Unreadable(format!(
            "scanner could not read it ({scanner_err}) and no engine is available"
        ));
    };
    // The scanner's size and NUL refusals must not be bypassed by the engine: NUL ends
    // tokenizing early in pwsh and would hide the commands after it.
    if let Some(why) = engine::refused_input(src) {
        return PwshReading::Unreadable(format!(
            "scanner could not read it ({scanner_err}); engine not asked: {why}"
        ));
    }
    match engine.names(src) {
        Ok(names) => PwshReading::Names {
            names,
            source: Source::Engine,
        },
        Err(e) => PwshReading::Unreadable(format!(
            "scanner could not read it ({scanner_err}); engine failed: {e:?}"
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PwshReading {
    /// Names came from the scanner or from the engine.
    Names { names: Vec<String>, source: Source },
    /// Neither engine could read it: the caller must ask for confirmation.
    Unreadable(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Scanner,
    Engine,
}
