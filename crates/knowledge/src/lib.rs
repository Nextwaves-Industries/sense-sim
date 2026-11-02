//! Engineering knowledge base for the Sense Sim assistant.
//!
//! Documents are Markdown with a small frontmatter; every `##` section becomes
//! one retrievable chunk. Retrieval is BM25 over weighted fields, with term
//! expansion for engineering synonyms and Vietnamese terms. It needs no model
//! download and no network, and its ranking is covered by `tests/eval.rs`.

pub mod api;
pub mod doc;
pub mod search;
pub mod text;

pub use doc::{Chunk, Doc, LoadReport, load_dir, parse};
pub use search::{Hit, Index};

include!(concat!(env!("OUT_DIR"), "/corpus.rs"));

/// The documents shipped inside the app.
pub fn builtin_docs() -> Vec<Doc> {
    CORPUS.iter().map(|(path, md)| parse(path, md, true)).collect()
}
