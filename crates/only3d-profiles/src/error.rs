use std::path::PathBuf;

use crate::store::PresetKind;
use crate::template::TemplateError;

/// Errors from loading/resolving presets and building G-code contexts.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in preset file {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("preset file {path} is not a JSON object")]
    NotAnObject { path: String },
    #[error("no Bambu profile directory (with machine/, process/, filament/) at {0}")]
    NotAProfileDir(PathBuf),
    #[error("{kind} preset `{name}` not found")]
    NotFound { kind: PresetKind, name: String },
    #[error("{kind} preset `{name}` inherits from `{parent}`, which does not exist")]
    MissingParent { kind: PresetKind, name: String, parent: String },
    #[error("{kind} preset `{name}` includes `{include}`, which does not exist")]
    MissingInclude { kind: PresetKind, name: String, include: String },
    #[error("inheritance/include cycle while resolving {kind} preset `{name}`")]
    Cycle { kind: PresetKind, name: String },
    #[error("key `{key}` is missing or invalid{context}")]
    BadKey { key: String, context: String },
    #[error("invalid job description: {0}")]
    InvalidJob(String),
    #[error(transparent)]
    Template(#[from] TemplateError),
}

impl ProfileError {
    pub(crate) fn bad_key(key: &str, context: impl std::fmt::Display) -> Self {
        let context = context.to_string();
        ProfileError::BadKey {
            key: key.to_string(),
            context: if context.is_empty() { context } else { format!(" ({context})") },
        }
    }
}
