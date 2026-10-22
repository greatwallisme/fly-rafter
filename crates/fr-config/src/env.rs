//! Layered `.env` resolution for Fly Rafter startup (PRD 6.4,
//! REQ-FR-CONFIG-13; 2026-10-22 Q11-Q14 rulings).
//!
//! Behavior is aligned 1:1 with dsh `loadLayeredEnv` (app-boot
//! `src/index.ts:238-259`) and `readEnvLayer` (`:193-225`), and the `.env`
//! grammar is the Node v22 `Dotenv::ParseContent` dialect that backs
//! `util.parseEnv` (`src/node_dotenv.cc`), pinned by the verbatim upstream
//! fixture under `tests/fixtures/dotenv-valid.env`.
//!
//! fr-config performs no process mutation itself: the crate is compiled with
//! `#![forbid(unsafe_code)]` and Rust 2024 marks `std::env::set_var` unsafe.
//! [`EnvSink`] abstracts the materialization target so the whole layered
//! resolution stays testable here; the host crate installs the process-env
//! adapter (a three-line `unsafe` block) as its first startup action, before
//! any thread or async runtime is spawned (PRD 6.4 dual-track rule).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::error::Error;

/// `.env` file name within a layer directory (dsh `readEnvLayer`).
pub const ENV_FILENAME: &str = ".env";
/// Environment override for the Fly Rafter home (dsh `DSH_HOME` brand map).
pub const FR_HOME_ENV: &str = "FR_HOME";
/// Directory name of the default Fly Rafter home under the OS home (dsh
/// `DSH_HOME_DIR_NAME = ".dsh"` brand map).
pub const FR_HOME_DIR_NAME: &str = ".fly-rafter";

/// Exact names no discovered `.env` file may set (dsh `BOOTSTRAP_NAMES`,
/// app-boot `src/index.ts:136-157`, 49 entries verbatim; `DEEPSEEK_*` keeps
/// its name because the provider itself is unchanged).
pub const BOOTSTRAP_NAMES: [&str; 49] = [
    "PATH",
    "HOME",
    "USERPROFILE",
    "SHELL",
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_EXTRA_CA_CERTS",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "BASH_ENV",
    "ENV",
    "SHELLOPTS",
    "BASHOPTS",
    "PERL5OPT",
    "PERL5LIB",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "RUBYOPT",
    "RUBYLIB",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "PYTHONHOME",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "GIT_EDITOR",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "BROWSER",
    "DEEPSEEK_BASE_URL",
    "DEEPSEEK_SEARCH_BASE_URL",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "NODE_TLS_REJECT_UNAUTHORIZED",
];

/// Name prefixes no discovered `.env` file may set (dsh
/// `BOOTSTRAP_PREFIXES`, app-boot `src/index.ts:160`; `DSH_` brand-mapped to
/// `FR_`, the other three carried over verbatim).
pub const BOOTSTRAP_PREFIXES: [&str; 4] = ["FR_", "XDG_", "DYLD_", "BASH_FUNC_"];

/// Proxy names only the Fly Rafter home `.env` may set (dsh
/// `HOME_LAYER_PROXY_NAMES`, app-boot `src/index.ts:169`).
///
/// A proxy chooses the route every request takes, so the invoking
/// directory's file — which arrives with a clone — keeps refusing them,
/// while the home file is the user's own and `FR_HOME` is itself
/// bootstrap-only, so no `.env` can relocate this exemption.
pub const HOME_LAYER_PROXY_NAMES: [&str; 4] =
    ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"];

/// Whether a variable may come only from the inherited process environment
/// because it changes process, runtime, VCS, or network bootstrap (dsh
/// `isBootstrapOnly`, case-insensitive).
#[must_use]
pub fn is_bootstrap_only(name: &str) -> bool {
    let upper = name.to_uppercase();
    BOOTSTRAP_NAMES.contains(&upper.as_str())
        || BOOTSTRAP_PREFIXES.iter().any(|p| upper.starts_with(p))
}

/// Node `trim_spaces` (`node_dotenv.cc`): strips only spaces, tabs, and
/// newlines — never other whitespace.
fn trim_spaces(input: &str) -> &str {
    input.trim_matches([' ', '\t', '\n'])
}

/// Drop everything through the next newline, or clear the rest (node
/// `ParseContent` line skipping).
fn skip_to_next_line(content: &str) -> &str {
    content.find('\n').map_or("", |nl| &content[nl + 1..])
}

/// Node double-quote fast path: replace every literal backslash-`n` with a
/// real newline, rescanning one byte past each replacement (`pos += 1`), so
/// `\\n` degrades to a backslash followed by a newline exactly like the C++
/// replace loop.
fn expand_double_quoted_newlines(value: &str) -> String {
    let mut out = value.to_owned();
    let mut pos = 0;
    while let Some(at) = out[pos..].find("\\n") {
        let at = pos + at;
        out.replace_range(at..at + 2, "\n");
        pos = at + 1;
    }
    out
}

/// Parse `.env` content under the Node `util.parseEnv` dialect (v22.x
/// `Dotenv::ParseContent`).
///
/// All `\r` removed first, full-line and bare `#` lines skipped, `export `
/// prefixes stripped, single-, double-, and backtick-quoted values (double
/// quotes expand literal `\n` and may span lines), unquoted values cut at
/// the first `#` with no space required, and the last assignment of a
/// duplicate key wins.
#[must_use]
pub fn parse_env(input: &str) -> BTreeMap<String, String> {
    parse_env_entries(input).into_iter().collect()
}

/// Parse `.env` content into `(name, value)` pairs in file order (same
/// dialect as [`parse_env`]).
///
/// dsh validates each layer by iterating `Object.keys` of the parsed
/// object (app-boot `src/index.ts` `readEnvLayer`), so the first
/// bootstrap-only name in file order — not in sorted order — is the one a
/// refusal names; duplicate keys keep their first position, and collecting
/// into a map yields the last-assignment-wins view.
fn parse_env_entries(input: &str) -> Vec<(String, String)> {
    let flat = input.replace('\r', "");
    let mut content: &str = trim_spaces(&flat);
    let mut store = Vec::new();
    while !content.is_empty() {
        if content.starts_with('\n') || content.starts_with('#') {
            content = skip_to_next_line(content);
            continue;
        }
        let Some(eq) = content.find(['=', '\n']) else {
            break;
        };
        if content.as_bytes()[eq] == b'\n' {
            content = trim_spaces(&content[eq + 1..]);
            continue;
        }
        let mut key = trim_spaces(&content[..eq]);
        content = &content[eq + 1..];
        if content.is_empty() || content.starts_with('\n') {
            store.push((key.to_owned(), String::new()));
            continue;
        }
        content = trim_spaces(content);
        if key.is_empty() {
            continue;
        }
        if let Some(rest) = key.strip_prefix("export ") {
            key = trim_spaces(rest);
        }
        if content.is_empty() {
            store.push((key.to_owned(), String::new()));
            break;
        }
        if content.starts_with('"')
            && let Some(close) = content[1..].find('"')
        {
            let value = expand_double_quoted_newlines(&content[1..=close]);
            store.push((key.to_owned(), value));
            content = skip_to_next_line(&content[close + 2..]);
            continue;
        }
        if content.starts_with('"') || content.starts_with('\'') || content.starts_with('`') {
            let quote = content.as_bytes()[0] as char;
            if let Some(close) = content[1..].find(quote) {
                store.push((key.to_owned(), content[1..=close].to_owned()));
                content = skip_to_next_line(&content[close + 2..]);
                continue;
            }
            if let Some(nl) = content.find('\n') {
                store.push((key.to_owned(), content[..nl].to_owned()));
                content = &content[nl + 1..];
            } else {
                store.push((key.to_owned(), content.to_owned()));
                break;
            }
        } else if let Some(nl) = content.find('\n') {
            let line = &content[..nl];
            let value = line.find('#').map_or(line, |hash| &line[..hash]);
            store.push((key.to_owned(), trim_spaces(value).to_owned()));
            content = &content[nl + 1..];
        } else {
            let value = content.find('#').map_or(content, |hash| &content[..hash]);
            store.push((key.to_owned(), trim_spaces(value).to_owned()));
            break;
        }
        content = trim_spaces(content);
    }
    store
}

/// One parsed `.env` layer: the file it came from and every entry it
/// declares (validated, not yet materialized).
#[derive(Debug, Clone)]
pub struct EnvLayer {
    /// Absolute path of the `.env` file behind this layer.
    pub path: PathBuf,
    /// All parsed entries of the layer, keys as written, in file order.
    ///
    /// Duplicate keys keep their first position and take the last
    /// assignment's value — the first-position/last-value shape of a JS
    /// object — so a case-colliding pair resolves to the file-later
    /// spelling exactly like `new Map(Object.entries(...))` in dsh
    /// `createLaunchEnvironmentSnapshot` (2026-10-22 Q22A1).
    pub values: IndexMap<String, String>,
}

/// Parse one directory's `.env` without applying it (dsh `readEnvLayer`).
///
/// Bootstrap-only names are rejected before any value is materialized: a
/// missing file is a silent empty layer, any other read failure reports one
/// warn line and skips the layer, and a bootstrap violation refuses startup.
///
/// `bin` is the diagnostic prefix (`fr`), `dir` the layer directory, `home`
/// the resolved Fly Rafter home (proxy exemption), and `warn` the one-line
/// diagnostic sink (receives the line without a trailing newline).
///
/// # Errors
///
/// Returns [`Error::EnvBootstrap`] when the layer declares a bootstrap-only
/// name this layer may not set (fail-closed; startup must terminate).
pub fn read_env_layer(
    bin: &str,
    dir: &Path,
    home: &Path,
    warn: &dyn Fn(&str),
) -> Result<Option<EnvLayer>, Error> {
    let path = dir.join(ENV_FILENAME);
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            warn(&format!(
                "{bin}: failed to load .env: {}: {err}",
                path.display()
            ));
            return Ok(None);
        }
    };
    let entries = parse_env_entries(&content);
    let is_home = dir == home;
    for (name, _) in &entries {
        if !is_bootstrap_only(name) {
            continue;
        }
        let proxy = HOME_LAYER_PROXY_NAMES.contains(&name.to_uppercase().as_str());
        if is_home && proxy {
            continue;
        }
        let remedy = if proxy {
            format!(
                "export {name}, or put it in {}, which does not travel with a repository",
                home.join(ENV_FILENAME).display()
            )
        } else {
            format!("export {name} instead of putting it in a .env file")
        };
        return Err(Error::EnvBootstrap(format!(
            "{bin}: {} sets \"{name}\", which only the launching environment may set (it decides \
             how this process starts, where its code and instructions load from, or how it \
             reaches the network); {remedy}",
            path.display()
        )));
    }
    Ok(Some(EnvLayer {
        path,
        values: entries.into_iter().collect(),
    }))
}

/// Which layer supplied a value, ordered most to least trusted (dsh
/// `LaunchEnvironmentSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EnvSource {
    /// The environment this process inherited.
    Process,
    /// The invoking directory's `.env`.
    ProjectEnv,
    /// The Fly Rafter home's `.env`.
    UserEnv,
}

/// Canonical trust order, most trusted first (dsh `SOURCE_ORDER`).
const SOURCE_ORDER: [EnvSource; 3] = [
    EnvSource::Process,
    EnvSource::ProjectEnv,
    EnvSource::UserEnv,
];

/// One resolved variable and the layer it came from (dsh
/// `LaunchEnvironmentEntry`).
#[derive(Debug, Clone)]
pub struct EnvEntry {
    /// The value as the supplying layer declared it.
    pub value: String,
    /// The layer that supplied the value.
    pub source: EnvSource,
    /// Absolute path of the file that supplied it; absent for
    /// [`EnvSource::Process`].
    pub path: Option<PathBuf>,
}

/// Fold a variable name to its lookup key: Windows folds environment names
/// case-insensitively, every other platform is exact (dsh `lookupKey`).
fn lookup_key(name: &str) -> Cow<'_, str> {
    if cfg!(windows) {
        Cow::Owned(name.to_uppercase())
    } else {
        Cow::Borrowed(name)
    }
}

/// Re-key one layer's entries into the folded lookup-key space.
///
/// Duplicate folded keys keep their first position and take the file-later
/// value — the collect into [`IndexMap`] reproduces the JS `Map` shape of
/// `new Map(Object.entries(...))` in dsh `createLaunchEnvironmentSnapshot`
/// (launch-environment src/index.ts:85), so on Windows a same-layer
/// `Path`/`PATH` collision resolves to the file-later spelling. The fold
/// function is injected so the collision path executes in tests on every
/// platform; the native win32 arm of [`lookup_key`] itself is covered only
/// by native Windows builds (dsh marks the same arm `v8 ignore`).
fn fold_keys(
    values: impl IntoIterator<Item = (String, String)>,
    fold: impl Fn(&str) -> Cow<'_, str>,
) -> IndexMap<String, String> {
    values
        .into_iter()
        .map(|(name, value)| (fold(&name).into_owned(), value))
        .collect()
}

/// The frozen environment of one launch: immutable layer contents that later
/// `chdir`, workspace switches, or resumed sessions resolve against (dsh
/// `LaunchEnvironmentSnapshot`).
#[derive(Debug, Clone)]
pub struct EnvSnapshot {
    process: IndexMap<String, String>,
    project: Option<EnvLayer>,
    user: Option<EnvLayer>,
}

impl EnvSnapshot {
    /// Fold stored keys through [`lookup_key`] on both sides of a lookup.
    ///
    /// dsh folds at store time (launch-environment
    /// `createLaunchEnvironmentSnapshot`), so a query and the stored entries
    /// meet in the same folded space on platforms with case-folded
    /// environment names. Duplicate folded keys keep first position and take
    /// the file-later value (see [`fold_keys`]).
    fn from_layers(
        process: BTreeMap<String, String>,
        project: Option<EnvLayer>,
        user: Option<EnvLayer>,
    ) -> Self {
        Self {
            process: fold_keys(process, lookup_key),
            project: project.map(|mut layer| {
                layer.values = fold_keys(layer.values, lookup_key);
                layer
            }),
            user: user.map(|mut layer| {
                layer.values = fold_keys(layer.values, lookup_key);
                layer
            }),
        }
    }

    /// Resolve one name across every layer, most trusted first.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<EnvEntry> {
        self.get_from(name, &SOURCE_ORDER)
    }

    /// Layer contents in canonical order, with the file path behind
    /// non-process layers.
    fn layer_of(&self, source: EnvSource) -> Option<(&IndexMap<String, String>, Option<&Path>)> {
        match source {
            EnvSource::Process => Some((&self.process, None)),
            EnvSource::ProjectEnv => self
                .project
                .as_ref()
                .map(|l| (&l.values, Some(l.path.as_path()))),
            EnvSource::UserEnv => self
                .user
                .as_ref()
                .map(|l| (&l.values, Some(l.path.as_path()))),
        }
    }

    /// Resolve one name only from `sources`, retaining the canonical trust
    /// order; omitted layers are unreachable.
    #[must_use]
    pub fn get_from(&self, name: &str, sources: &[EnvSource]) -> Option<EnvEntry> {
        let key = lookup_key(name);
        for source in SOURCE_ORDER {
            if !sources.contains(&source) {
                continue;
            }
            if let Some((values, path)) = self.layer_of(source)
                && let Some(value) = values.get(key.as_ref())
            {
                return Some(EnvEntry {
                    value: value.clone(),
                    source,
                    path: path.map(Path::to_path_buf),
                });
            }
        }
        None
    }
}

/// Materialization target for accepted `.env` values.
///
/// The production sink mutates the process environment; tests substitute a
/// map (dsh writes `process.env` directly — the sink seam exists only
/// because Rust 2024 marks `set_var` unsafe and this crate forbids
/// `unsafe`).
pub trait EnvSink {
    /// Snapshot of the inherited environment, taken before any layer is
    /// applied (dsh `{ ...process.env }`).
    fn inherited(&self) -> BTreeMap<String, String>;

    /// Whether the target currently holds a value for `name` (an empty
    /// value counts as set, dsh `=== undefined` check).
    fn is_set(&self, name: &str) -> bool;

    /// Store `value` for `name`; only called for names not yet set.
    fn set(&mut self, name: &str, value: &str);
}

/// Load the layered environment: inherited process env > invoking-directory
/// `.env` > Fly Rafter home `.env` (dsh `loadLayeredEnv`).
///
/// Both files are parsed and validated before either is applied, so a
/// rejection cannot leave one file applied; the home layer is not read at
/// all when `cwd` is the home; accepted values materialize through `sink`
/// without replacing a higher-ranked name; the returned snapshot records
/// which layer supplied each value.
///
/// `cwd` and `home` must be resolved absolute paths (as produced by
/// [`resolve_home`]).
///
/// # Errors
///
/// Returns [`Error::EnvBootstrap`] when either layer declares a
/// bootstrap-only variable, except the home-layer proxy names.
pub fn load_layered_env_into(
    bin: &str,
    cwd: &Path,
    home: &Path,
    warn: &dyn Fn(&str),
    sink: &mut impl EnvSink,
) -> Result<EnvSnapshot, Error> {
    let inherited = sink.inherited();
    let project = read_env_layer(bin, cwd, home, warn)?;
    let user = if cwd == home {
        None
    } else {
        read_env_layer(bin, home, home, warn)?
    };
    for layer in [project.as_ref(), user.as_ref()].into_iter().flatten() {
        for (name, value) in &layer.values {
            if !sink.is_set(name) {
                sink.set(name, value);
            }
        }
    }
    Ok(EnvSnapshot::from_layers(inherited, project, user))
}

/// Resolve the Fly Rafter home from the ambient environment (dsh
/// `resolveDshHome`): highest precedence first — an explicit `configured`
/// path, a non-blank `$FR_HOME`, then `~/.fly-rafter` under the OS home.
///
/// # Errors
///
/// Propagates the OS home or path-resolution failure.
pub fn resolve_home(configured: Option<&Path>) -> std::io::Result<PathBuf> {
    let fr_home = std::env::var(FR_HOME_ENV).ok();
    resolve_home_from(configured, fr_home.as_deref())
}

/// [`resolve_home`] with the `FR_HOME` value injected for testability.
///
/// A blank (whitespace-only) value counts as unset, while a non-blank value
/// is used as written — trim only decides blankness (dsh `resolveRoot`).
///
/// # Errors
///
/// Propagates the OS home or path-resolution failure.
pub fn resolve_home_from(
    configured: Option<&Path>,
    fr_home: Option<&str>,
) -> std::io::Result<PathBuf> {
    let selected = match configured {
        Some(path) => path.to_path_buf(),
        None => {
            if let Some(value) = fr_home.filter(|value| !value.trim().is_empty()) {
                PathBuf::from(value)
            } else {
                let os_home = std::env::home_dir()
                    .ok_or_else(|| std::io::Error::other("cannot resolve the OS home directory"))?;
                os_home.join(FR_HOME_DIR_NAME)
            }
        }
    };
    std::path::absolute(&expand_home_path(&selected)?)
}

/// Expand the supported tilde prefixes (`~`, `~/`, `~\`) against the OS home
/// (dsh `expandHomePath`); any other path passes through unchanged.
///
/// # Errors
///
/// Propagates the OS home resolution failure.
fn expand_home_path(path: &Path) -> std::io::Result<PathBuf> {
    let text = path.to_string_lossy();
    if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
        let home = std::env::home_dir()
            .ok_or_else(|| std::io::Error::other("cannot resolve the OS home directory"))?;
        return Ok(if text.len() == 1 {
            home
        } else {
            home.join(&text[2..])
        });
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Map-backed sink standing in for the process environment.
    struct MapSink {
        values: BTreeMap<String, String>,
    }

    impl MapSink {
        fn new(entries: &[(&str, &str)]) -> Self {
            Self {
                values: entries
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
            }
        }
    }

    impl EnvSink for MapSink {
        fn inherited(&self) -> BTreeMap<String, String> {
            self.values.clone()
        }

        fn is_set(&self, name: &str) -> bool {
            self.values.contains_key(name)
        }

        fn set(&mut self, name: &str, value: &str) {
            self.values.insert(name.to_owned(), value.to_owned());
        }
    }

    /// Unique temp directory standing in for a layer directory or home.
    struct TempLayer {
        path: PathBuf,
    }

    impl TempLayer {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("fr-config-env-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write_env(&self, content: &str) {
            std::fs::write(self.path.join(ENV_FILENAME), content).unwrap();
        }
    }

    impl Drop for TempLayer {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    // Node's own test/parallel/test-util-parse-env.js bluebook: the verbatim
    // valid.env fixture and every expected pair.
    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dotenv-valid.env"
    ));

    const EXPECTED: [(&str, &str); 45] = [
        ("A", "B=C"),
        ("AFTER_LINE", "after_line"),
        ("B", "C=D"),
        ("BACKTICKS", "backticks"),
        (
            "BACKTICKS_INSIDE_DOUBLE",
            "`backticks` work inside double quotes",
        ),
        (
            "BACKTICKS_INSIDE_SINGLE",
            "`backticks` work inside single quotes",
        ),
        ("BACKTICKS_SPACED", "    backticks    "),
        ("BASIC", "basic"),
        ("DONT_EXPAND_SQUOTED", "dontexpand\\nnewlines"),
        ("DONT_EXPAND_UNQUOTED", "dontexpand\\nnewlines"),
        (
            "DOUBLE_AND_SINGLE_QUOTES_INSIDE_BACKTICKS",
            "double \"quotes\" and single 'quotes' work inside backticks",
        ),
        ("DOUBLE_QUOTES", "double_quotes"),
        (
            "DOUBLE_QUOTES_INSIDE_BACKTICKS",
            "double \"quotes\" work inside backticks",
        ),
        (
            "DOUBLE_QUOTES_INSIDE_SINGLE",
            "double \"quotes\" work inside single quotes",
        ),
        ("DOUBLE_QUOTES_SPACED", "    double quotes    "),
        (
            "DOUBLE_QUOTES_WITH_NO_SPACE_BRACKET",
            "{ port: $MONGOLAB_PORT}",
        ),
        ("EDGE_CASE_INLINE_COMMENTS", "VALUE1"),
        ("EMAIL", "therealnerdybeast@example.tld"),
        ("EMPTY", ""),
        ("EMPTY_BACKTICKS", ""),
        ("EMPTY_DOUBLE_QUOTES", ""),
        ("EMPTY_SINGLE_QUOTES", ""),
        ("EQUAL_SIGNS", "equals=="),
        ("EXPORT_EXAMPLE", "ignore export"),
        ("EXPAND_NEWLINES", "expand\nnew\nlines"),
        ("INLINE_COMMENTS", "inline comments"),
        (
            "INLINE_COMMENTS_BACKTICKS",
            "inline comments outside of #backticks",
        ),
        (
            "INLINE_COMMENTS_DOUBLE_QUOTES",
            "inline comments outside of #doublequotes",
        ),
        (
            "INLINE_COMMENTS_SINGLE_QUOTES",
            "inline comments outside of #singlequotes",
        ),
        ("INLINE_COMMENTS_SPACE", "inline comments start with a"),
        ("MULTI_BACKTICKED", "THIS\nIS\nA\n\"MULTILINE'S\"\nSTRING"),
        ("MULTI_DOUBLE_QUOTED", "THIS\nIS\nA\nMULTILINE\nSTRING"),
        ("MULTI_NOT_VALID", "THIS"),
        ("MULTI_NOT_VALID_QUOTE", "\""),
        ("MULTI_SINGLE_QUOTED", "THIS\nIS\nA\nMULTILINE\nSTRING"),
        ("RETAIN_INNER_QUOTES", "{\"foo\": \"bar\"}"),
        ("RETAIN_INNER_QUOTES_AS_BACKTICKS", "{\"foo\": \"bar's\"}"),
        ("RETAIN_INNER_QUOTES_AS_STRING", "{\"foo\": \"bar\"}"),
        ("SINGLE_QUOTES", "single_quotes"),
        (
            "SINGLE_QUOTES_INSIDE_BACKTICKS",
            "single 'quotes' work inside backticks",
        ),
        (
            "SINGLE_QUOTES_INSIDE_DOUBLE",
            "single 'quotes' work inside double quotes",
        ),
        ("SINGLE_QUOTES_SPACED", "    single quotes    "),
        ("SPACED_KEY", "parsed"),
        ("SPACE_BEFORE_DOUBLE_QUOTES", "space before double quotes"),
        ("TRIM_SPACE_FROM_UNQUOTED", "some spaced out string"),
    ];

    #[test]
    fn parser_matches_node_parse_env_official_fixture() {
        let parsed = parse_env(FIXTURE);
        assert_eq!(parsed.len(), EXPECTED.len());
        for (key, value) in EXPECTED {
            assert_eq!(parsed.get(key), Some(&value.to_owned()), "key {key}");
        }
    }

    #[test]
    fn parser_duplicate_keys_keep_last_value() {
        let parsed = parse_env("FOO=bar\nFOO=baz\n");
        assert_eq!(parsed.get("FOO").map(String::as_str), Some("baz"));
    }

    #[test]
    fn parser_empty_input_yields_empty_map() {
        assert!(parse_env("").is_empty());
    }

    #[test]
    fn parser_double_quote_escape_expansion_including_backslash_quirk() {
        let parsed = parse_env("A=\"x\\ny\"\nB=\"\\\\n\"\n");
        assert_eq!(parsed.get("A").map(String::as_str), Some("x\ny"));
        // Node rescans one byte past each replacement, so a literal
        // backslash before `n` degrades to backslash + real newline.
        assert_eq!(parsed.get("B").map(String::as_str), Some("\\\n"));
    }

    #[test]
    fn parser_strips_every_carriage_return_before_parsing() {
        let parsed = parse_env("A=B\r\nC='x\ry'\r\n");
        assert_eq!(parsed.get("A").map(String::as_str), Some("B"));
        assert_eq!(parsed.get("C").map(String::as_str), Some("xy"));
    }

    #[test]
    fn parser_export_prefix_and_spaced_key_forms() {
        let parsed = parse_env("export A=1\nexport   B=2\nexportC=3\n  D  =  4  \n");
        assert_eq!(parsed.get("A").map(String::as_str), Some("1"));
        assert_eq!(parsed.get("B").map(String::as_str), Some("2"));
        // `exportC` has no space so the prefix is not stripped.
        assert_eq!(parsed.get("exportC").map(String::as_str), Some("3"));
        assert_eq!(parsed.get("D").map(String::as_str), Some("4"));
    }

    #[test]
    fn parser_valueless_keys_and_invalid_lines() {
        let parsed = parse_env("EMPTY=\nNOEQ LINE\n=v\nTAIL");
        assert_eq!(parsed.get("EMPTY").map(String::as_str), Some(""));
        assert_eq!(parsed.len(), 1);
        let parsed = parse_env("KEY=");
        assert_eq!(parsed.get("KEY").map(String::as_str), Some(""));
    }

    #[test]
    fn parser_unquoted_hash_starts_comment_without_space() {
        let parsed = parse_env("A=v # cut\nB=w#cut\nC=\"k # kept\"\n");
        assert_eq!(parsed.get("A").map(String::as_str), Some("v"));
        assert_eq!(parsed.get("B").map(String::as_str), Some("w"));
        assert_eq!(parsed.get("C").map(String::as_str), Some("k # kept"));
    }

    #[test]
    fn parser_double_quote_fast_path_and_unterminated_line_form() {
        // The double-quote fast path ends at the FIRST closing quote in the
        // remaining content and everything after it up to the next newline
        // is skipped; the capture itself may span real newlines (verified
        // against node v24.22: {"A":"value\nB=2\nC="} — that input carries a
        // single closing quote, while `A="x" B="y"` pins the first-quote
        // rule differentially).
        let parsed = parse_env("A=\"value\nB=2\nC=\"");
        assert_eq!(parsed.get("A").map(String::as_str), Some("value\nB=2\nC="));
        assert!(!parsed.contains_key("B"));
        assert!(!parsed.contains_key("C"));
        // With no later closing quote, the value is the rest of the line
        // including the quote character and parsing continues (node:
        // {"Q":"\"","R":"2"}).
        let parsed = parse_env("Q=\"\nR=2\n");
        assert_eq!(parsed.get("Q").map(String::as_str), Some("\""));
        assert_eq!(parsed.get("R").map(String::as_str), Some("2"));
    }

    #[test]
    fn bootstrap_names_and_prefixes_case_insensitive() {
        for name in [
            "PATH",
            "path",
            "ENV",
            "NODE_EXTRA_CA_CERTS",
            "REQUESTS_CA_BUNDLE",
        ] {
            assert!(is_bootstrap_only(name), "{name}");
        }
        for prefix in ["FR_X", "XDG_A", "DYLD_B", "BASH_FUNC_C"] {
            assert!(is_bootstrap_only(prefix), "{prefix}");
        }
        // dsh has no NODE_ prefix: only the exact NODE_ names are rejected.
        assert!(!is_bootstrap_only("NODE_FOO"));
        assert!(!is_bootstrap_only("EDITOR_TOOLS"));
    }

    #[test]
    fn snapshot_resolves_in_trust_order_with_provenance() {
        let process = MapSink::new(&[("ONLY_PROC", "p"), ("SHARED", "proc")]);
        let project = EnvLayer {
            path: PathBuf::from("/w/.env"),
            values: IndexMap::from([
                ("SHARED".to_owned(), "proj".to_owned()),
                ("ONLY_PROJECT".to_owned(), "proj".to_owned()),
            ]),
        };
        let user = EnvLayer {
            path: PathBuf::from("/h/.env"),
            values: IndexMap::from([
                ("SHARED".to_owned(), "user".to_owned()),
                ("ONLY_PROJECT".to_owned(), "user".to_owned()),
            ]),
        };
        let snapshot = EnvSnapshot::from_layers(process.inherited(), Some(project), Some(user));
        let shared = snapshot.get("SHARED").unwrap();
        assert_eq!(shared.value, "proc");
        assert_eq!(shared.source, EnvSource::Process);
        assert_eq!(shared.path, None);
        // get_from restricts the reachable layers but keeps trust order.
        let shared = snapshot
            .get_from("SHARED", &[EnvSource::UserEnv, EnvSource::ProjectEnv])
            .unwrap();
        assert_eq!(shared.value, "proj");
        assert_eq!(shared.source, EnvSource::ProjectEnv);
        assert_eq!(shared.path.as_deref(), Some(Path::new("/w/.env")));
        // An omitted layer is unreachable: the user-layer copy answers a
        // user-only query; a process-only name has no project/user answer.
        let only_user = snapshot
            .get_from("ONLY_PROJECT", &[EnvSource::UserEnv])
            .unwrap();
        assert_eq!(only_user.value, "user");
        assert_eq!(only_user.source, EnvSource::UserEnv);
        assert_eq!(only_user.path.as_deref(), Some(Path::new("/h/.env")));
        assert!(
            snapshot
                .get_from("ONLY_PROC", &[EnvSource::ProjectEnv, EnvSource::UserEnv])
                .is_none()
        );
    }

    #[test]
    fn layered_three_layer_precedence_and_no_override() {
        let project = TempLayer::new("prec-proj");
        let home = TempLayer::new("prec-home");
        project.write_env("A=from-project\nB=from-project\nC=from-project\n");
        home.write_env("A=from-home\nB=from-home\nD=from-home\n");
        let mut sink = MapSink::new(&[("A", "from-process"), ("B", "")]);
        let warns = std::cell::RefCell::new(Vec::new());
        let warn = |line: &str| warns.borrow_mut().push(line.to_owned());
        let snapshot =
            load_layered_env_into("fr", &project.path, &home.path, &warn, &mut sink).unwrap();
        // Process wins over both files, an empty process value still counts
        // as set, project wins over home, and home fills what nobody set.
        assert_eq!(
            sink.values.get("A").map(String::as_str),
            Some("from-process")
        );
        assert_eq!(sink.values.get("B").map(String::as_str), Some(""));
        assert_eq!(
            sink.values.get("C").map(String::as_str),
            Some("from-project")
        );
        assert_eq!(sink.values.get("D").map(String::as_str), Some("from-home"));
        assert!(warns.borrow().is_empty());
        // The snapshot records the layer of each value even when the value
        // was not materialized because a higher layer held the name.
        assert_eq!(snapshot.get("C").unwrap().source, EnvSource::ProjectEnv);
        assert_eq!(snapshot.get("D").unwrap().source, EnvSource::UserEnv);
    }

    #[test]
    fn layered_atomic_rejection_applies_neither_file() {
        let project = TempLayer::new("atomic-proj");
        let home = TempLayer::new("atomic-home");
        project.write_env("GOOD=yes\n");
        home.write_env("FR_SESSIONS_ROOT=/x\n");
        let mut sink = MapSink::new(&[]);
        let warns = std::cell::RefCell::new(Vec::new());
        let warn = |line: &str| warns.borrow_mut().push(line.to_owned());
        let error =
            load_layered_env_into("fr", &project.path, &home.path, &warn, &mut sink).unwrap_err();
        assert!(matches!(error, Error::EnvBootstrap(_)));
        // A rejection must not leave one file applied.
        assert!(sink.values.is_empty());
        assert!(warns.borrow().is_empty());
    }

    #[test]
    fn layered_proxy_exemption_only_in_home_layer() {
        let project = TempLayer::new("proxy-proj");
        let home = TempLayer::new("proxy-home");
        project.write_env("HTTPS_PROXY=http://p:1\n");
        let mut sink = MapSink::new(&[]);
        let error =
            load_layered_env_into("fr", &project.path, &home.path, &|_| {}, &mut sink).unwrap_err();
        let message = match &error {
            Error::EnvBootstrap(message) => message.clone(),
            other => panic!("unexpected error: {other:?}"),
        };
        // The proxy remedy points at the home file as the second way out.
        assert!(message.contains(&home.path.join(ENV_FILENAME).display().to_string()));
        assert!(message.contains("does not travel with a repository"));
        assert!(sink.values.is_empty());
        // The same four names pass in the home layer.
        home.write_env(
            "HTTP_PROXY=http://h:1\nHTTPS_PROXY=http://h:1\nALL_PROXY=socks://h\nNO_PROXY=a,b\n",
        );
        project.write_env("PLAIN=1\n");
        let mut sink = MapSink::new(&[]);
        load_layered_env_into("fr", &project.path, &home.path, &|_| {}, &mut sink).unwrap();
        assert_eq!(
            sink.values.get("HTTPS_PROXY").map(String::as_str),
            Some("http://h:1")
        );
    }

    #[test]
    fn layered_case_insensitive_and_prefix_rejections() {
        for content in [
            "path=/evil\n",
            "XDG_CONFIG_HOME=/evil\n",
            "DYLD_LIB=x\n",
            "BASH_FUNC_x=()\n",
        ] {
            let project = TempLayer::new("case-proj");
            let home = TempLayer::new("case-home");
            project.write_env(content);
            let mut sink = MapSink::new(&[]);
            assert!(
                load_layered_env_into("fr", &project.path, &home.path, &|_| {}, &mut sink).is_err(),
                "content {content}"
            );
            assert!(sink.values.is_empty());
        }
    }

    #[test]
    fn layered_cwd_home_dedup_reads_directory_once() {
        let dir = TempLayer::new("dedup");
        dir.write_env("A=1\n");
        let mut sink = MapSink::new(&[]);
        load_layered_env_into("fr", &dir.path, &dir.path, &|_| {}, &mut sink).unwrap();
        // One layer supplied the value; the home layer was not read again.
        assert_eq!(sink.values.get("A").map(String::as_str), Some("1"));
        let snapshot =
            load_layered_env_into("fr", &dir.path, &dir.path, &|_| {}, &mut MapSink::new(&[]))
                .unwrap();
        assert_eq!(snapshot.get("A").unwrap().source, EnvSource::ProjectEnv);
    }

    #[test]
    fn layer_missing_file_is_silent_and_read_error_warns_then_skips() {
        let project = TempLayer::new("io-proj");
        let home = TempLayer::new("io-home");
        let mut sink = MapSink::new(&[]);
        let warns = std::cell::RefCell::new(Vec::new());
        let warn = |line: &str| warns.borrow_mut().push(line.to_owned());
        load_layered_env_into("fr", &project.path, &home.path, &warn, &mut sink).unwrap();
        // ENOENT on both layers: silent, nothing materialized.
        assert!(warns.borrow().is_empty());
        assert!(sink.values.is_empty());
        // A non-ENOENT failure (directory in place of the file) warns once
        // and skips the layer without refusing startup.
        std::fs::create_dir(project.path.join(ENV_FILENAME)).unwrap();
        let mut sink = MapSink::new(&[]);
        let warns = std::cell::RefCell::new(Vec::new());
        let warn = |line: &str| warns.borrow_mut().push(line.to_owned());
        load_layered_env_into("fr", &project.path, &home.path, &warn, &mut sink).unwrap();
        assert_eq!(warns.borrow().len(), 1);
        assert!(warns.borrow()[0].starts_with("fr: failed to load .env: "));
        assert!(sink.values.is_empty());
    }

    #[test]
    fn non_proxy_bootstrap_violation_names_the_export_remedy() {
        let project = TempLayer::new("remedy-proj");
        let home = TempLayer::new("remedy-home");
        project.write_env("PATH=/evil\n");
        let mut sink = MapSink::new(&[]);
        let error =
            load_layered_env_into("fr", &project.path, &home.path, &|_| {}, &mut sink).unwrap_err();
        let message = match &error {
            Error::EnvBootstrap(message) => message,
            other => panic!("unexpected error: {other:?}"),
        };
        assert!(message.starts_with("fr: "));
        assert!(message.contains("sets \"PATH\""));
        assert!(message.contains("export PATH instead of putting it in a .env file"));
        assert!(!message.contains("does not travel"));
    }

    #[test]
    fn bootstrap_violation_names_first_in_file_order() {
        // dsh iterates Object.keys in file insertion order and refuses on
        // the first violating name (app-boot src/index.ts readEnvLayer);
        // sorted-key iteration would name PATH for both files.
        for (content, named) in [
            ("VISUAL=vim\nPATH=/evil\n", "VISUAL"),
            ("PATH=/evil\nVISUAL=vim\n", "PATH"),
        ] {
            let project = TempLayer::new("order-proj");
            let home = TempLayer::new("order-home");
            project.write_env(content);
            let mut sink = MapSink::new(&[]);
            let error = load_layered_env_into("fr", &project.path, &home.path, &|_| {}, &mut sink)
                .unwrap_err();
            let message = match &error {
                Error::EnvBootstrap(message) => message,
                other => panic!("unexpected error: {other:?}"),
            };
            let needle = format!("sets \"{named}\"");
            assert!(
                message.contains(needle.as_str()),
                "content {content}: {message}"
            );
        }
    }

    #[test]
    fn layer_keeps_file_order_and_last_duplicate_value() {
        // dsh layer values keep the JS object shape: file order, duplicate
        // keys at their first position with the last assignment's value.
        let project = TempLayer::new("fileorder-proj");
        let home = TempLayer::new("fileorder-home");
        project.write_env("B=1\nA=2\nFOO=1\nFOO=2\n");
        let layer = read_env_layer("fr", &project.path, &home.path, &|_| {})
            .unwrap()
            .expect("project layer must exist");
        assert_eq!(
            layer.values.keys().map(String::as_str).collect::<Vec<_>>(),
            ["B", "A", "FOO"]
        );
        assert_eq!(layer.values.get("FOO").map(String::as_str), Some("2"));
    }

    #[test]
    fn fold_keys_case_collision_keeps_file_later_value_at_first_position() {
        // On Windows lookup_key uppercases, so `Path` and `PATH` collide;
        // the collect keeps the first position and the file-later value (dsh
        // createLaunchEnvironmentSnapshot, launch-environment src/index.ts:85).
        // The fold function is injected so the collision path executes on
        // every platform; the native win32 arm of lookup_key is covered only
        // by native Windows builds (dsh marks the same arm `v8 ignore`).
        let entries = IndexMap::from([
            ("Path".to_owned(), "a".to_owned()),
            ("PATH".to_owned(), "b".to_owned()),
        ]);
        let folded = fold_keys(entries.clone(), |name| Cow::Owned(name.to_uppercase()));
        assert_eq!(
            folded.keys().map(String::as_str).collect::<Vec<_>>(),
            ["PATH"]
        );
        assert_eq!(folded.get("PATH").map(String::as_str), Some("b"));
        // Without folding, both spellings stay distinct, in file order.
        let exact = fold_keys(entries, |name| Cow::Borrowed(name));
        assert_eq!(
            exact.keys().map(String::as_str).collect::<Vec<_>>(),
            ["Path", "PATH"]
        );
    }

    #[test]
    fn snapshot_layer_case_variants_stay_distinct_off_windows() {
        // lookup_key is identity off Windows: the two spellings never
        // collide and each resolves to its own value.
        let layer = EnvLayer {
            path: PathBuf::from("/w/.env"),
            values: IndexMap::from([
                ("Path".to_owned(), "a".to_owned()),
                ("PATH".to_owned(), "b".to_owned()),
            ]),
        };
        let snapshot = EnvSnapshot::from_layers(BTreeMap::new(), Some(layer), None);
        assert_eq!(snapshot.get("Path").unwrap().value, "a");
        assert_eq!(snapshot.get("PATH").unwrap().value, "b");
    }

    #[test]
    fn resolve_home_precedence_tilde_and_blank_env() {
        let os_home = std::env::home_dir().unwrap();
        // Explicit configuration beats everything, and `~` expands.
        assert_eq!(
            resolve_home_from(Some(Path::new("~/cfg")), None).unwrap(),
            os_home.join("cfg")
        );
        // FR_HOME beats the default; a non-blank value is used as written —
        // a leading space makes it cwd-relative, and no trimming happens
        // (dsh resolveRoot trims only for the blankness test).
        assert_eq!(
            resolve_home_from(None, Some("  /data/fr  ")).unwrap(),
            std::path::absolute("  /data/fr  ").unwrap()
        );
        // A blank FR_HOME counts as unset and falls back to the default.
        assert_eq!(
            resolve_home_from(None, Some("   ")).unwrap(),
            os_home.join(".fly-rafter")
        );
        assert_eq!(
            resolve_home_from(None, None).unwrap(),
            os_home.join(".fly-rafter")
        );
        // Relative selections resolve to absolute paths.
        assert!(
            resolve_home_from(Some(Path::new("rel")), None)
                .unwrap()
                .is_absolute()
        );
    }
}
