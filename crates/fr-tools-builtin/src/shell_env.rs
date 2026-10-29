// Rust guideline compliant 2026-10-29
//! The managed `FR_*` shell-environment registry (dsh `shell-env`,
//! verbatim refusal texts and collection order under the Q16a-1A FR_
//! prefix ruling).
//!
//! The namespace is rebuilt for every model shell call: ambient `FR_*`
//! values are discarded by the executor, then the registry's current
//! snapshot is injected after the ordinary environment (the fr-shell
//! request's `fr_env` slot). Built-in shell facts stay registry-owned;
//! plugins contribute additional, enumerable facts with ownership
//! declared up front so conflicts surface before the first command.
//!
//! The five built-in keys mirror dsh's built-ins: `FR_HOME`,
//! `FR_SHELL`, `FR_SESSION_ID`, `FR_PROFILE`, `FR_PROFILE_DIR`. The
//! profile pair rides the profile context, which no M1 composition
//! mounts — the pair is absent from every snapshot, and the absence is
//! the pinned behavior (the dsh `profileContext` slot is equally
//! optional).

use std::collections::BTreeMap;

use serde_json::Value;

/// The managed-variable prefix (the dsh `DSH_ENV_PREFIX` slot under the
/// fr naming ruling).
pub const ENV_PREFIX: &str = "FR_";

/// The home-directory fact (dsh `DSH_HOME`): the harness home exposed to
/// every model shell.
pub const HOME_KEY: &str = "FR_HOME";
/// The shell-presence fact (dsh `DSH_SHELL`): marks a managed shell.
pub const SHELL_KEY: &str = "FR_SHELL";
/// The calling-session fact (dsh `DSH_SESSION_ID`).
pub const SESSION_ID_KEY: &str = "FR_SESSION_ID";
/// The active-profile fact (dsh `DSH_PROFILE`): absent while no profile
/// context is mounted (the pinned M1 absence).
pub const PROFILE_KEY: &str = "FR_PROFILE";
/// The active-profile directory fact (dsh `DSH_PROFILE_DIR`): absent
/// with [`PROFILE_KEY`].
pub const PROFILE_DIR_KEY: &str = "FR_PROFILE_DIR";

/// The five reserved keys (dsh `RESERVED_BASH_ENV_KEYS`).
///
/// They are the built-ins no contributor may own.
#[must_use]
pub fn reserved_keys() -> Vec<&'static str> {
    vec![
        HOME_KEY,
        SHELL_KEY,
        SESSION_ID_KEY,
        PROFILE_KEY,
        PROFILE_DIR_KEY,
    ]
}

/// The key-shape rule after the prefix (dsh `BASH_ENV_KEY_SUFFIX`):
/// uppercase letters, digits, and underscores, led by a letter.
fn valid_key_suffix(suffix: &str) -> bool {
    let mut chars = suffix.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Model-visible metadata for one managed variable (dsh
/// `BashEnvVariable`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVariable {
    /// Concise description of the environment fact represented by the
    /// variable.
    pub description: String,
}

/// A contribution to the managed environment of each model shell call
/// (dsh `BashEnvContributor`).
///
/// Declared keys make ownership conflicts detectable before the first
/// command; `resolve` computes only the values available for the
/// current execution.
pub trait EnvContributor: Send + Sync {
    /// Stable contributor name used in diagnostics and duplicate
    /// detection.
    fn name(&self) -> &str;
    /// The complete set of `FR_*` keys this contributor may return,
    /// with their model-visible metadata.
    fn variables(&self) -> &BTreeMap<String, EnvVariable>;
    /// Resolves this contributor's available values for one execution.
    ///
    /// # Errors
    ///
    /// A contributor whose resolution can fail for reasons the
    /// collection contract cannot absorb (dsh contributors return
    /// partial maps; the fr face keeps the same partial-map shape, so
    /// the error arm stays for trait-object wrapping).
    fn resolve(&self, execution: &Execution) -> Result<BTreeMap<String, String>, String>;
}

/// The execution context a collection reads (the dsh `ToolExecution`
/// projection the registry needs: the calling agent, when any).
#[derive(Debug, Clone, Default)]
pub struct Execution {
    /// The calling agent's session identity, when the call has one.
    pub agent: Option<String>,
}

/// An enumerable declaration (dsh `BashEnvVariableInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVariableInfo {
    /// Contributor that owns the variable.
    pub contributor: String,
    /// Description of the fact the variable represents.
    pub description: String,
    /// Declared `FR_*` environment variable name.
    pub key: String,
}

/// The registry of trusted, per-execution `FR_*` variables (dsh
/// `ShellEnvRegistry`).
pub struct ShellEnvRegistry {
    contributors: BTreeMap<String, Box<dyn EnvContributor>>,
    key_owners: BTreeMap<String, String>,
    home: String,
}

impl std::fmt::Debug for ShellEnvRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellEnvRegistry")
            .field(
                "contributors",
                &self.contributors.keys().collect::<Vec<_>>(),
            )
            .field("home", &self.home)
            .finish_non_exhaustive()
    }
}

impl ShellEnvRegistry {
    /// Creates the registry over the resolved home directory (dsh
    /// `resolveDshHome(config.dshHome)`: the explicit override, then
    /// the environment variable, then the dot-directory in the user
    /// home).
    ///
    /// # Errors
    ///
    /// The home resolution failure (no override, no `FR_HOME`, and no
    /// resolvable user home).
    pub fn new(home_override: Option<&str>) -> Result<Self, String> {
        Ok(Self {
            contributors: BTreeMap::new(),
            key_owners: BTreeMap::new(),
            home: resolve_home(home_override)?,
        })
    }

    /// The resolved home directory the `FR_HOME` fact carries.
    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    /// Registers one contributor (dsh `register`, verbatim refusal
    /// texts). Names and keys are unique; built-in keys are reserved.
    ///
    /// # Errors
    ///
    /// The dsh refusal family: an empty name, a duplicate name, an
    /// invalid or reserved or duplicated key, an undescribed key.
    pub fn register(&mut self, contributor: Box<dyn EnvContributor>) -> Result<(), String> {
        let name = contributor.name().to_owned();
        if name.trim().is_empty() {
            return Err("bash env contributor name must be non-empty".to_owned());
        }
        if self.contributors.contains_key(&name) {
            return Err(format!(
                "bash env contributor \"{name}\" is already registered"
            ));
        }
        for (key, variable) in contributor.variables() {
            if !key.starts_with(ENV_PREFIX) || !valid_key_suffix(&key[ENV_PREFIX.len()..]) {
                return Err(format!(
                    "bash env contributor \"{name}\" declared invalid key \"{key}\""
                ));
            }
            if reserved_keys().contains(&key.as_str()) {
                return Err(format!(
                    "bash env contributor \"{name}\" cannot own reserved key \"{key}\""
                ));
            }
            if variable.description.trim().is_empty() {
                return Err(format!(
                    "bash env contributor \"{name}\" must describe \"{key}\""
                ));
            }
            if let Some(owner) = self.key_owners.get(key) {
                return Err(format!(
                    "bash env key \"{key}\" is already owned by contributor \"{owner}\"; \
contributor \"{name}\" cannot also own it"
                ));
            }
        }
        for key in contributor.variables().keys() {
            self.key_owners.insert(key.clone(), name.clone());
        }
        self.contributors.insert(name, contributor);
        Ok(())
    }

    /// Builds the trusted `FR_*` snapshot for one shell execution (dsh
    /// `collect`): the built-ins, then each contributor's partial map in
    /// name order, the whole snapshot frozen in key order.
    ///
    /// # Errors
    ///
    /// A contributor returning an undeclared key or a non-string value
    /// (the dsh `collect` refusals, raised mid-build).
    pub fn collect(&self, execution: &Execution) -> Result<FrozenEnv, String> {
        let mut values = BTreeMap::new();
        values.insert(HOME_KEY.to_owned(), self.home.clone());
        values.insert(SHELL_KEY.to_owned(), "1".to_owned());
        if let Some(agent) = &execution.agent {
            values.insert(SESSION_ID_KEY.to_owned(), agent.clone());
        }
        // The profile pair stays absent: no M1 composition mounts a
        // profile context, and the absence is the pinned behavior.

        // dsh iterates contributors in `localeCompare` name order; the
        // BTreeMap iteration is that order for the ASCII names the
        // registry admits.
        for contributor in self.contributors.values() {
            let resolved = contributor.resolve(execution)?;
            for (raw_key, value) in resolved {
                if !contributor.variables().contains_key(&raw_key) {
                    return Err(format!(
                        "bash env contributor \"{}\" returned undeclared key \"{raw_key}\"",
                        contributor.name()
                    ));
                }
                values.insert(raw_key, value);
            }
        }
        Ok(FrozenEnv(values))
    }

    /// Enumerates the plugin-contributed variables without executing
    /// their resolvers, sorted by key (dsh `list`; the built-ins are
    /// registry-owned and stay unlisted, matching the dsh module's
    /// documented status quo).
    #[must_use]
    pub fn list(&self) -> Vec<EnvVariableInfo> {
        let mut infos: Vec<EnvVariableInfo> = self
            .contributors
            .values()
            .flat_map(|contributor| {
                contributor
                    .variables()
                    .iter()
                    .map(move |(key, variable)| EnvVariableInfo {
                        contributor: contributor.name().to_owned(),
                        description: variable.description.clone(),
                        key: key.clone(),
                    })
            })
            .collect();
        infos.sort_by(|left, right| left.key.cmp(&right.key));
        infos
    }
}

/// The frozen per-execution snapshot (dsh `DshEnvironment`): a
/// key-ordered, immutable map handed to the executor request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenEnv(BTreeMap<String, String>);

impl FrozenEnv {
    /// The ordered key-value pairs (the dsh `Object.entries` order the
    /// executor merges after the ordinary environment).
    #[must_use]
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// The JSON object form for the shell request wire (`fr_env`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        )
    }
}

/// Resolves the harness home (dsh `resolveDshHome`): the explicit
/// override, then `FR_HOME`, then `~/.fr` under the user home.
///
/// # Errors
///
/// No override, no environment variable, and no resolvable user home.
fn resolve_home(override_home: Option<&str>) -> Result<String, String> {
    if let Some(home) = override_home {
        return Ok(home.to_owned());
    }
    if let Ok(home) = std::env::var("FR_HOME") {
        return Ok(home);
    }
    std::env::var("HOME")
        .map_err(|error| format!("shell env: no harness home available (set FR_HOME): {error}"))
        .map(|user_home| format!("{user_home}/.fr"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One scripted contributor.
    struct ScriptContributor {
        name: String,
        variables: BTreeMap<String, EnvVariable>,
        values: BTreeMap<String, String>,
    }

    impl EnvContributor for ScriptContributor {
        fn name(&self) -> &str {
            &self.name
        }

        fn variables(&self) -> &BTreeMap<String, EnvVariable> {
            &self.variables
        }

        fn resolve(&self, _execution: &Execution) -> Result<BTreeMap<String, String>, String> {
            Ok(self.values.clone())
        }
    }

    fn contributor(name: &str, keys: &[&str]) -> ScriptContributor {
        let variables = keys
            .iter()
            .map(|key| {
                (
                    (*key).to_owned(),
                    EnvVariable {
                        description: "fact".to_owned(),
                    },
                )
            })
            .collect();
        let values = keys
            .iter()
            .map(|key| ((*key).to_owned(), "v".to_owned()))
            .collect();
        ScriptContributor {
            name: name.to_owned(),
            variables,
            values,
        }
    }

    fn registry() -> ShellEnvRegistry {
        ShellEnvRegistry::new(Some("/harness-home")).expect("home")
    }

    /// A contributor that returns a key outside its declaration.
    struct UndeclaredContributor;

    impl EnvContributor for UndeclaredContributor {
        fn name(&self) -> &'static str {
            "rogue"
        }

        fn variables(&self) -> &BTreeMap<String, EnvVariable> {
            static VARS: std::sync::OnceLock<BTreeMap<String, EnvVariable>> =
                std::sync::OnceLock::new();
            VARS.get_or_init(|| {
                BTreeMap::from([(
                    "FR_OK".to_owned(),
                    EnvVariable {
                        description: "fact".to_owned(),
                    },
                )])
            })
        }

        fn resolve(&self, _execution: &Execution) -> Result<BTreeMap<String, String>, String> {
            Ok(BTreeMap::from([("FR_ROGUE".to_owned(), "v".to_owned())]))
        }
    }

    /// The built-ins ride every snapshot; the profile pair stays absent
    /// without a mounted profile context.
    #[test]
    fn collect_carries_the_built_ins_and_pins_the_profile_absence() {
        let registry = registry();
        let snapshot = registry
            .collect(&Execution {
                agent: Some("s1".to_owned()),
            })
            .expect("collect");
        let pairs = snapshot.pairs();
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["FR_HOME", "FR_SESSION_ID", "FR_SHELL"]);
        let by_key: std::collections::HashMap<&str, &str> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(by_key["FR_HOME"], "/harness-home");
        assert_eq!(by_key["FR_SESSION_ID"], "s1");
        assert_eq!(by_key["FR_SHELL"], "1");
        // An agent-less execution drops the session fact.
        let bare = registry.collect(&Execution::default()).expect("collect");
        let bare_pairs = bare.pairs();
        let bare_keys: Vec<&str> = bare_pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(bare_keys, vec!["FR_HOME", "FR_SHELL"]);
    }

    /// The eight registration refusals ride the dsh texts verbatim.
    #[test]
    fn register_refusals_pin_the_dsh_texts() {
        let mut registry = registry();
        assert_eq!(
            registry
                .register(Box::new(contributor(" ", &["FR_A"])))
                .unwrap_err(),
            "bash env contributor name must be non-empty"
        );
        registry
            .register(Box::new(contributor("dup", &["FR_A"])))
            .expect("first");
        assert_eq!(
            registry
                .register(Box::new(contributor("dup", &["FR_B"])))
                .unwrap_err(),
            "bash env contributor \"dup\" is already registered"
        );
        assert_eq!(
            registry
                .register(Box::new(contributor("bad", &["FR_lower"])))
                .unwrap_err(),
            "bash env contributor \"bad\" declared invalid key \"FR_lower\""
        );
        assert_eq!(
            registry
                .register(Box::new(contributor("bad", &["NOPREFIX"])))
                .unwrap_err(),
            "bash env contributor \"bad\" declared invalid key \"NOPREFIX\""
        );
        assert_eq!(
            registry
                .register(Box::new(contributor("bad", &[HOME_KEY])))
                .unwrap_err(),
            format!("bash env contributor \"bad\" cannot own reserved key \"{HOME_KEY}\"")
        );
        // An undescribed key refuses after the shape checks.
        let undescribed = ScriptContributor {
            name: "undescribed".to_owned(),
            variables: BTreeMap::from([(
                "FR_A".to_owned(),
                EnvVariable {
                    description: "  ".to_owned(),
                },
            )]),
            values: BTreeMap::new(),
        };
        assert_eq!(
            registry.register(Box::new(undescribed)).unwrap_err(),
            "bash env contributor \"undescribed\" must describe \"FR_A\""
        );
        assert_eq!(
            registry
                .register(Box::new(contributor("second", &["FR_A"])))
                .unwrap_err(),
            "bash env key \"FR_A\" is already owned by contributor \"dup\"; contributor \
\"second\" cannot also own it"
        );
    }

    /// The collection refusals fire for undeclared keys; valid snapshots
    /// order contributor values after the built-ins and the list face
    /// sorts by key.
    #[test]
    fn collect_refuses_undeclared_keys_and_list_sorts() {
        let mut registry = registry();
        registry
            .register(Box::new(contributor("one", &["FR_B"])))
            .expect("one");
        registry
            .register(Box::new(contributor("two", &["FR_A"])))
            .expect("two");
        let snapshot = registry.collect(&Execution::default()).expect("collect");
        let pairs = snapshot.pairs();
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        // Built-ins first, then the contributors' keys in frozen order.
        assert_eq!(keys, vec!["FR_A", "FR_B", "FR_HOME", "FR_SHELL"]);
        let listed = registry.list();
        let listed_keys: Vec<&str> = listed.iter().map(|info| info.key.as_str()).collect();
        assert_eq!(listed_keys, vec!["FR_A", "FR_B"]);
        assert!(
            listed
                .iter()
                .all(|info| info.contributor == "one" || info.contributor == "two")
        );

        let mut rogue_registry = ShellEnvRegistry::new(Some("/harness-home")).expect("home");
        rogue_registry
            .register(Box::new(UndeclaredContributor))
            .expect("register");
        assert_eq!(
            rogue_registry.collect(&Execution::default()).unwrap_err(),
            "bash env contributor \"rogue\" returned undeclared key \"FR_ROGUE\""
        );
    }
}
