//! Plugin artifact resolution (PRD 6.2): patch row names map to dylib/wasm
//! artifact paths; the loader `builtin` table is never produced by fr.

use std::path::{Path, PathBuf};

/// Loader source kind of a resolved plugin artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// C-ABI dylib plugin (all first-party plugins are dylibs).
    Dylib,
    /// Sandboxed wasm component (third-party sandbox extensions only).
    Wasm,
}

impl SourceKind {
    /// The `source.kind` string emitted into the loader document.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dylib => "dylib",
            Self::Wasm => "wasm",
        }
    }
}

/// A resolved plugin artifact: the loader `source` object payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    /// Resolved artifact kind.
    pub kind: SourceKind,
    /// Resolved artifact path.
    pub path: PathBuf,
}

/// Resolves patch row plugin names to artifacts per the PRD 6.2 rule.
///
/// The rule: a name containing a path separator or ending in `.so`/`.wasm`
/// is a literal path; otherwise the slug (`-` replaced by `_`) is looked up
/// as `lib<slug>.so` (dylib) then `<slug>.wasm` (wasm) in each search
/// directory in order; the first hit decides the kind.
#[derive(Debug, Clone)]
pub struct PathResolver {
    /// Search directories in priority order.
    dirs: Vec<PathBuf>,
}

impl PathResolver {
    /// Creates a resolver over the given search directories (priority
    /// order). The host supplies `FR_PLUGIN_PATH` entries and the install
    /// library directory.
    #[must_use]
    pub const fn new(dirs: Vec<PathBuf>) -> Self {
        Self { dirs }
    }

    /// Resolves one plugin name to its artifact.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when no artifact is found for a
    /// non-literal name (the row then lands in compile failures).
    pub fn resolve(&self, name: &str) -> std::result::Result<SourceSpec, String> {
        // Extension comparison is deliberately case-sensitive: artifact file
        // names on the target filesystems are case-sensitive (PRD 6.2 rule).
        let extension = Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let is_wasm = extension == "wasm";
        let is_dylib = extension == "so";
        if name.contains('/') || is_dylib || is_wasm {
            let kind = if is_wasm {
                SourceKind::Wasm
            } else {
                SourceKind::Dylib
            };
            return Ok(SourceSpec {
                kind,
                path: PathBuf::from(name),
            });
        }
        let slug = name.replace('-', "_");
        for dir in &self.dirs {
            let dylib = dir.join(format!("lib{slug}.so"));
            if dylib.is_file() {
                return Ok(SourceSpec {
                    kind: SourceKind::Dylib,
                    path: dylib,
                });
            }
            let wasm = dir.join(format!("{slug}.wasm"));
            if wasm.is_file() {
                return Ok(SourceSpec {
                    kind: SourceKind::Wasm,
                    path: wasm,
                });
            }
        }
        Err(format!(
            "no artifact found for plugin name `{name}` in {} search dir(s)",
            self.dirs.len()
        ))
    }
}

/// Validates one resolved row against the trust boundary (PRD 4.3 / CONFIG-8):
/// first-party plugin names must resolve to dylib sources; wasm is reserved
/// for third-party sandbox extensions.
///
/// `is_first_party` is supplied by the host (it owns the first-party plugin
/// name set); fr-config only applies the policy.
#[must_use]
pub fn check_trust_boundary(
    name: &str,
    spec: &SourceSpec,
    is_first_party: &dyn Fn(&str) -> bool,
) -> Option<String> {
    (is_first_party(name) && spec.kind == SourceKind::Wasm).then(|| {
        format!(
            "trust boundary violation: first-party plugin `{name}` resolved to wasm artifact {}",
            spec.path.display()
        )
    })
}
