//! The pwsh oracle: one resident parser, restarted at most `MAX_RESTARTS` times per run.

use nucleos_base::command_reader::pwsh::engine::{EngineError, ResidentParser};
use std::path::PathBuf;

pub const MAX_RESTARTS: u32 = 3;

pub enum OracleOut {
    Names(Vec<String>),
    /// The Parser refused the text (parse errors): the case is invalid PowerShell.
    Rejected(String),
}

pub struct Oracle {
    exe: PathBuf,
    parser: Option<ResidentParser>,
    pub restarts: u32,
}

impl Oracle {
    pub fn new(exe: PathBuf) -> Self {
        let parser = ResidentParser::start(&exe)
            .unwrap_or_else(|e| panic!("cannot start the pwsh oracle at {}: {e}", exe.display()));
        Oracle {
            exe,
            parser: Some(parser),
            restarts: 0,
        }
    }

    fn restart(&mut self, why: &str) {
        self.parser = None; // Drop kills the old child.
        if self.restarts >= MAX_RESTARTS {
            panic!("pwsh oracle died more than {MAX_RESTARTS} times (last: {why})");
        }
        self.restarts += 1;
        match ResidentParser::start(&self.exe) {
            Ok(p) => self.parser = Some(p),
            Err(e) => panic!("cannot restart the pwsh oracle: {e} (after: {why})"),
        }
    }

    pub fn names(&mut self, src: &str) -> OracleOut {
        loop {
            let res = match self.parser.as_mut() {
                Some(p) => p.command_names(src),
                None => Err(EngineError::Down("no parser".into())),
            };
            match res {
                Ok(v) => return OracleOut::Names(v),
                Err(EngineError::ParseErrors(n)) => {
                    return OracleOut::Rejected(format!("{n} parse error(s)"));
                }
                Err(EngineError::Down(m)) => self.restart(&format!("down: {m}")),
                Err(EngineError::Timeout) => {
                    // A text that hangs the oracle is not comparable: restart and skip it.
                    self.restart("timeout");
                    return OracleOut::Rejected("oracle timeout".into());
                }
                Err(EngineError::Protocol(m)) => {
                    let alive = self.parser.as_mut().map(|p| p.is_alive()).unwrap_or(false);
                    if alive {
                        return OracleOut::Rejected(format!("protocol: {m}"));
                    }
                    self.restart(&format!("protocol while dead: {m}"));
                }
            }
        }
    }
}
