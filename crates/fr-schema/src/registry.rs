//! Registry of first-party plugin config schemas.
//!
//! Declarations are centralized in this library (PRD 6.3): the host consults
//! the registry during assembly without loading any plugin, and each plugin
//! artifact links this same crate to fetch its declaration for activation-side
//! re-validation. Entries are appended as plugin crates land (decision 20:
//! the M1 framework ships with the lookup in place and real schemas attach
//! per crate).

use crate::schema::Schema;

/// Registry of first-party plugin schemas, keyed by plugin slug.
static PLUGIN_SCHEMAS: &[(&str, &Schema)] = &[];

/// Looks up the schema declared for `plugin`, or [`None`] when absent.
///
/// Assembly skips config validation for rows without a declaration (the dsh
/// `absent` semantics): third-party artifacts have no host-side schema and
/// re-validate inside their own artifact instead.
#[must_use]
pub fn lookup(plugin: &str) -> Option<&'static Schema> {
    PLUGIN_SCHEMAS
        .iter()
        .find(|(name, _)| *name == plugin)
        .map(|(_, schema)| *schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_plugin_slug_is_never_registered() {
        // The empty slug is rejected by profile and patch name validation
        // alike, so it must stay absent from the registry as entries land.
        assert!(lookup("").is_none());
    }
}
