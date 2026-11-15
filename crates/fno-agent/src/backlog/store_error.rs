//! The typed failure of the SQLite backlog store (`backlog/mod.rs`,
//! `backlog/nodes.rs`). Every variant displays exactly the text the store
//! printed when it returned `String`, so a refusal line reads the same; the
//! context rides in the variant, not only in the words.

use crate::backlog::model::ModelError;

#[derive(Debug, thiserror::Error)]
pub enum StoreDbError {
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Model(#[from] ModelError),
    /// The busy retry spent its budget. `cause` is the last busy failure.
    #[error(
        "graph write refused: the store was busy after {attempts} attempts over \
         {elapsed_secs:.1}s (sqlite: {cause}). Nothing was written; no node was created. \
         Run `fno backlog get <id>` to confirm, then retry."
    )]
    Busy {
        attempts: usize,
        elapsed_secs: f32,
        cause: Box<StoreDbError>,
    },
    #[error("{0}")]
    Graph(#[from] Box<crate::graph_store::StoreError>),
    /// A refusal the store states in words: a missing version, a bad
    /// stored value, or a caller's apply closure saying no.
    #[error("{0}")]
    Refused(String),
}

impl StoreDbError {
    /// A busy or locked store. The text test stays: a version that moved
    /// under the write lock is reported as busy by words, not by an SQLite
    /// code, and the retry must treat both the same.
    pub fn is_busy(&self) -> bool {
        let text = self.to_string();
        text.contains("locked") || text.contains("busy")
    }
}

impl From<crate::graph_store::StoreError> for StoreDbError {
    fn from(error: crate::graph_store::StoreError) -> Self {
        Self::Graph(Box::new(error))
    }
}

impl From<String> for StoreDbError {
    fn from(text: String) -> Self {
        Self::Refused(text)
    }
}

impl From<&str> for StoreDbError {
    fn from(text: &str) -> Self {
        Self::Refused(text.to_string())
    }
}

impl From<StoreDbError> for String {
    fn from(error: StoreDbError) -> Self {
        error.to_string()
    }
}
