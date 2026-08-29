//! External resource resolution for `<include href="...">` (issue #2).
//!
//! This crate never reads files or the network itself — `Cargo.toml`/
//! `CLAUDE.md`'s "no host-format/I/O dependency in the core" policy
//! applies here too. A caller who wants `<include>` support implements
//! [`SchemaResolver`] over whatever loading mechanism (files, an
//! in-memory map, HTTP, ...) fits, with whatever security boundary
//! (allowed schemes, path restrictions) is appropriate there, and calls
//! [`crate::parse_with_resolver`] instead of [`crate::parse`]. Mirrors the
//! sister crate `relax-ng`'s `SchemaResolver`/`SchemaSource` shape (same
//! problem — external `href` resolution without owning I/O — same shape).
//!
//! `<extends href="...">` (external-file rule reuse, the other half of
//! issue #2) is *not* wired up to this trait yet — the ISO reference
//! implementation (`iso_dsdl_include.xsl`) itself labels `extends[@href]`
//! "experimental and non-standard", with different substitution semantics
//! than `<include>` (it splices the target element's *children*, not the
//! element itself, and shares `<include>`'s own `#fragment-id` lookup
//! complexity). Deferred rather than rushed — see `README.md`'s support
//! matrix.

use std::fmt;

/// Schema text plus the base URI relative `href`s inside it resolve
/// against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaSource {
    text: String,
    base_uri: String,
}

impl SchemaSource {
    /// Builds a source from its text and base URI (used to resolve any
    /// relative `href`s this schema fragment itself contains).
    pub fn new(text: impl Into<String>, base_uri: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            base_uri: base_uri.into(),
        }
    }

    /// The schema fragment's raw XML text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The base URI relative `href`s in this fragment resolve against.
    pub fn base_uri(&self) -> &str {
        &self.base_uri
    }
}

/// Resolves an `<include href="...">` `href` (relative to a base URI) to
/// the [`SchemaSource`] it names. This crate has no file-system or network
/// access of its own — implement this over whatever loading mechanism
/// fits the caller.
pub trait SchemaResolver {
    /// Resolves `href` (as it literally appears in the schema) relative to
    /// `base_uri`, or fails with a [`ResolveError`] if it can't be found,
    /// read, or is disallowed.
    fn resolve(&self, href: &str, base_uri: &str) -> Result<SchemaSource, ResolveError>;
}

/// A [`SchemaResolver`] failed to resolve an `href`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveError {
    message: String,
}

impl ResolveError {
    /// Builds a `ResolveError` with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ResolveError {}
