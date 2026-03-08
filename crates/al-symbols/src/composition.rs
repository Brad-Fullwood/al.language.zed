//! Extension composition — merge base + extensions into composed view.

/// A composed object (base + all extensions merged).
#[derive(Debug, Clone)]
pub struct ComposedObject {
    pub base: String,
    pub extensions: Vec<String>,
}
