use std::path::PathBuf;

/// Failure to parse, read, or resolve a role.
#[derive(Debug, thiserror::Error)]
pub enum RoleError {
    /// Malformed XML.
    #[error("not XML: {0}")]
    Xml(#[from] roxmltree::Error),
    /// Wrong root tag.
    #[error("the root element is not <role>")]
    NotARole,
    /// Missing role name.
    #[error("<role> has no name")]
    NoName,
    /// Unsupported schema content.
    #[error("role schema: {0}")]
    Schema(String),
    /// Invalid attribute value.
    #[error("{attribute}=\"{value}\" is not one of {allowed}")]
    Attribute {
        /// Attribute name.
        attribute: &'static str,
        /// Rejected value.
        value: String,
        /// Permitted values.
        allowed: &'static str,
    },
    /// Missing parent.
    #[error("no role is named \"{0}\"")]
    Missing(String),
    /// A cycle or a chain longer than eight roles, including base.
    #[error("\"{0}\" extends more than 8 roles deep, or in a loop")]
    TooDeep(String),
    /// A non-base role has no kind or tier.
    #[error("\"{0}\" and its parents name no kind or tier")]
    NoKind(String),
    /// Child widens parent permissions.
    #[error("role {0} widens its parent permissions")]
    Widening(String),
    /// File operation failed.
    #[error("reading role layer {path}: {source}")]
    Io {
        /// Failed path.
        path: PathBuf,
        /// Underlying disk error.
        #[source]
        source: std::io::Error,
    },
}
