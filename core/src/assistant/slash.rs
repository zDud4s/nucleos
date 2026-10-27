//! Slash commands: parsing, expansion, the registry and `/help`.
//!
//! A typed `/name arguments` is taken apart by `parse`, found by `registry` (the built-ins first,
//! then project, personal and plugin files) and either answered here as a card (`help`) or
//! expanded into the prompt every engine receives (`expand`). Nothing in this module talks to a
//! model or to a chat: the caller owns that, and `Outcome` says what it should do next.

pub mod expand;
pub mod help;
pub mod parse;
pub mod registry;

use crate::agents::capabilities::Engine;

/// What running a slash command asks of the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// An answer that stays in the chat. It is not a turn and creates no run.
    Card {
        kind: CardKind,
        data: serde_json::Value,
    },
    /// A prompt to send: `text` goes to the model, `typed` is what the transcript shows.
    Prompt { text: String, typed: String },
    /// A control the shell already has, for it to open.
    Open { control: Control },
}

/// Which card the shell draws for a `Outcome::Card`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CardKind {
    /// Every command, grouped by where it came from.
    Help,
    /// One command in full.
    HelpEntry,
}

/// The shell controls a command can open instead of changing state itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    ModelMenu,
    PermissionMenu,
    ProjectPicker,
    ChatHelpers,
}

/// Why a slash command was refused. Each variant is one HTTP answer, mapped by the route.
#[derive(Debug, Clone, PartialEq)]
pub enum SlashError {
    /// No such command: 404 `unknown_command`, with the names closest to what was typed.
    Unknown {
        name: String,
        suggestions: Vec<String>,
    },
    /// The arguments do not fit: 400 `invalid_argument`.
    InvalidArgument { usage: String, detail: String },
    /// The chat has a turn in flight and this changes state: 409 `busy`.
    Busy,
    /// The command needs a project the chat does not have: 409 `needs_project`.
    NeedsProject,
    /// This engine cannot do what the command asks: 422 `unsupported_engine`.
    Unsupported { engine: Engine, what: String },
    /// A workflow skill without what it runs on: 422 `workflow_unavailable`.
    WorkflowUnavailable { detail: String },
    /// Anything else: 500.
    Internal(String),
}
