//! Licence and provenance vocabularies (6.6.1 and 6.6.2).
//!
//! Two different axes, never scored on. `licence` is whether content may be
//! redistributed; `provenance` is who is speaking. The provenance values are
//! also normative for the v7 `ClusterSummary.provenance_class`
//! (`docs/v7-change-spec.md` §4), so changing them changes that spec.

pub const LICENCE_OWN: &str = "own";
pub const LICENCE_OPEN: &str = "open";
pub const LICENCE_RESTRICTED: &str = "restricted";
pub const LICENCE_UNKNOWN: &str = "unknown";

pub const LICENCE_CLASSES: [&str; 4] = [
    LICENCE_OWN,
    LICENCE_OPEN,
    LICENCE_RESTRICTED,
    LICENCE_UNKNOWN,
];

/// Stated directly by the human.
pub const PROVENANCE_ASSERTED: &str = "asserted";
/// The system's own reasoning or write-up of work done.
pub const PROVENANCE_CONCLUDED: &str = "concluded";
/// From an external source: an article, documentation, a page.
pub const PROVENANCE_RETRIEVED: &str = "retrieved";

pub const PROVENANCE_CLASSES: [&str; 3] = [
    PROVENANCE_RETRIEVED,
    PROVENANCE_CONCLUDED,
    PROVENANCE_ASSERTED,
];

/// The v7 default disclosure scope for a memory nobody has classified.
pub const DEFAULT_CLASSIFICATION: &str = r#"{"level":"internal","scopes":[]}"#;

/// Label given to RSS articles whose feed sets no project of its own.
pub const RSS_PROJECT_LABEL: &str = "RSS";
