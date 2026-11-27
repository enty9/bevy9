//! Structured script diagnostics and Console error reporting.

use std::collections::VecDeque;

use bevy_ecs::{entity::Entity, event::Event, resource::Resource};
use engine_core::EntityGuid;
use rhai::{EvalAltResult, ParseError, Position};
use serde::{Deserialize, Serialize};

/// Severity level of a [`ScriptDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptDiagnosticSeverity {
    /// Non-fatal warning raised during script loading or execution.
    Warning,
    /// Parse, compile, reload, sandbox, or runtime error.
    Error,
}

/// Pipeline stage that produced a [`ScriptDiagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptDiagnosticStage {
    /// Reading or resolving a script source file.
    Load,
    /// Parsing or compiling a script into an AST.
    Compile,
    /// Executing a script top-level body or lifecycle hook.
    Runtime,
    /// Hot-reloading a modified script on disk.
    Reload,
}

/// Structured error or warning captured from the scripting runtime without crashing the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptDiagnostic {
    /// Monotonically increasing diagnostic identifier.
    pub id: u64,
    /// Severity level.
    pub severity: ScriptDiagnosticSeverity,
    /// Stage where the diagnostic originated.
    pub stage: ScriptDiagnosticStage,
    /// Script path or logical identifier.
    pub file: String,
    /// 1-based line number inside `file`, if known.
    pub line: Option<u32>,
    /// 1-based column number inside `file`, if known.
    pub column: Option<u32>,
    /// ECS entity bits, when associated with a specific entity.
    pub entity_bits: Option<u64>,
    /// Entity `Name`, when associated with a named entity.
    pub entity_name: Option<String>,
    /// Persistent [`EntityGuid`], when associated with a scene entity.
    pub entity_guid: Option<EntityGuid>,
    /// Lifecycle hook name (for example `on_update`), if applicable.
    pub hook: Option<String>,
    /// Human-readable error message.
    pub message: String,
}

impl ScriptDiagnostic {
    /// Formats a concise one-line location string (`file:line:col` or `file`).
    #[must_use]
    pub fn location(&self) -> String {
        match (self.line, self.column) {
            (Some(line), Some(col)) => format!("{}:{line}:{col}", self.file),
            (Some(line), None) => format!("{}:{line}", self.file),
            _ => self.file.clone(),
        }
    }

    /// Formats a human-readable entity label (`Name (guid)` / `guid` / `Entity(...)`).
    #[must_use]
    pub fn entity_label(&self) -> Option<String> {
        match (&self.entity_name, self.entity_guid, self.entity_bits) {
            (Some(name), Some(guid), _) => Some(format!("{name} ({guid})")),
            (Some(name), None, Some(bits)) => Some(format!("{name} ({bits})")),
            (Some(name), None, None) => Some(name.clone()),
            (None, Some(guid), _) => Some(guid.to_string()),
            (None, None, Some(bits)) => Some(format!("entity#{bits}")),
            (None, None, None) => None,
        }
    }
}

/// Bevy observer event emitted whenever a [`ScriptDiagnostic`] is recorded.
#[derive(Event, Debug, Clone, PartialEq, Eq)]
pub struct ScriptDiagnosticEvent(pub ScriptDiagnostic);

/// Entity attribution metadata attached to a diagnostic.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticEntityContext {
    /// Active ECS entity.
    pub entity: Option<Entity>,
    /// Entity `Name` component value.
    pub entity_name: Option<String>,
    /// Entity persistent [`EntityGuid`].
    pub entity_guid: Option<EntityGuid>,
}

/// Default maximum number of diagnostics retained in [`ScriptDiagnostics`].
pub const DEFAULT_MAX_DIAGNOSTICS: usize = 256;

/// Bounded buffer of recent [`ScriptDiagnostic`] entries.
#[derive(Resource, Debug, Clone)]
pub struct ScriptDiagnostics {
    entries: VecDeque<ScriptDiagnostic>,
    capacity: usize,
    next_id: u64,
}

impl Default for ScriptDiagnostics {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MAX_DIAGNOSTICS)
    }
}

impl ScriptDiagnostics {
    /// Creates a diagnostics buffer retaining at most `capacity` entries (clamped to >= 1).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: capacity.max(1),
            next_id: 1,
        }
    }

    /// Records a diagnostic, logs it through `tracing` so the editor Console displays it,
    /// and returns the finalized [`ScriptDiagnostic`] (with assigned `id`).
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        severity: ScriptDiagnosticSeverity,
        stage: ScriptDiagnosticStage,
        file: impl Into<String>,
        line: Option<u32>,
        column: Option<u32>,
        entity_ctx: &DiagnosticEntityContext,
        hook: Option<String>,
        message: impl Into<String>,
    ) -> ScriptDiagnostic {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);

        let diag = ScriptDiagnostic {
            id,
            severity,
            stage,
            file: file.into(),
            line,
            column,
            entity_bits: entity_ctx.entity.map(Entity::to_bits),
            entity_name: entity_ctx.entity_name.clone(),
            entity_guid: entity_ctx.entity_guid,
            hook,
            message: message.into(),
        };

        log_diagnostic(&diag);

        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(diag.clone());
        diag
    }

    /// Returns all retained diagnostics in chronological order.
    #[must_use]
    pub fn entries(&self) -> Vec<ScriptDiagnostic> {
        self.entries.iter().cloned().collect()
    }

    /// Returns the most recently recorded diagnostic, if any.
    #[must_use]
    pub fn latest(&self) -> Option<&ScriptDiagnostic> {
        self.entries.back()
    }

    /// Returns all retained diagnostics associated with `entity`.
    #[must_use]
    pub fn for_entity(&self, entity: Entity) -> Vec<&ScriptDiagnostic> {
        let bits = entity.to_bits();
        self.entries
            .iter()
            .filter(|diag| diag.entity_bits == Some(bits))
            .collect()
    }

    /// Returns the number of retained diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no diagnostics are currently retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Removes and returns all retained diagnostics in chronological order.
    pub fn drain(&mut self) -> Vec<ScriptDiagnostic> {
        self.entries.drain(..).collect()
    }

    /// Clears all retained diagnostics.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

fn log_diagnostic(diag: &ScriptDiagnostic) {
    let location = diag.location();
    let entity = diag.entity_label().unwrap_or_else(|| "-".to_owned());
    let hook = diag.hook.as_deref().unwrap_or("-");
    match diag.severity {
        ScriptDiagnosticSeverity::Error => {
            tracing::error!(
                target: "engine_scripting",
                stage = ?diag.stage,
                file = %diag.file,
                line = ?diag.line,
                column = ?diag.column,
                entity = %entity,
                hook = %hook,
                "Script error at {location} [entity={entity}, hook={hook}]: {}",
                diag.message
            );
        }
        ScriptDiagnosticSeverity::Warning => {
            tracing::warn!(
                target: "engine_scripting",
                stage = ?diag.stage,
                file = %diag.file,
                line = ?diag.line,
                column = ?diag.column,
                entity = %entity,
                hook = %hook,
                "Script warning at {location} [entity={entity}, hook={hook}]: {}",
                diag.message
            );
        }
    }
}

/// Converts a Rhai [`Position`] into `(Option<u32>, Option<u32>)` 1-based `(line, column)`.
#[must_use]
pub fn position_to_line_col(pos: Position) -> (Option<u32>, Option<u32>) {
    let line = pos.line().and_then(|l| u32::try_from(l).ok());
    let col = pos.position().and_then(|c| u32::try_from(c).ok());
    (line, col)
}

/// Extracts `(line, column, message)` from a Rhai [`ParseError`].
#[must_use]
pub fn parse_error_details(err: &ParseError) -> (Option<u32>, Option<u32>, String) {
    let (line, col) = position_to_line_col(err.position());
    (line, col, err.err_type().to_string())
}

/// Extracts `(line, column, message, is_sandbox_limit)` from a Rhai [`EvalAltResult`].
#[must_use]
pub fn eval_error_details(err: &EvalAltResult) -> (Option<u32>, Option<u32>, String, bool) {
    let (inner, pos) = unwrap_eval_error(err);
    let (line, col) = position_to_line_col(pos);
    let sandbox_limit = matches!(
        inner,
        EvalAltResult::ErrorTooManyOperations(_)
            | EvalAltResult::ErrorStackOverflow(_)
            | EvalAltResult::ErrorDataTooLarge(_, _)
            | EvalAltResult::ErrorTooManyModules(_)
    );
    (line, col, inner.to_string(), sandbox_limit)
}

fn unwrap_eval_error(err: &EvalAltResult) -> (&EvalAltResult, Position) {
    match err {
        EvalAltResult::ErrorInFunctionCall(_, _, inner, outer_pos) => {
            let (leaf, inner_pos) = unwrap_eval_error(inner);
            let pos = if inner_pos.is_none() {
                *outer_pos
            } else {
                inner_pos
            };
            (leaf, pos)
        }
        EvalAltResult::ErrorInModule(_, inner, outer_pos) => {
            let (leaf, inner_pos) = unwrap_eval_error(inner);
            let pos = if inner_pos.is_none() {
                *outer_pos
            } else {
                inner_pos
            };
            (leaf, pos)
        }
        other => (other, other.position()),
    }
}
