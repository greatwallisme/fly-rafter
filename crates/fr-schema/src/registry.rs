// Rust guideline compliant 2026-10-30
//! Registry of first-party plugin config-schema declarations.
//!
//! Declarations are centralized in this library (PRD 6.3): the host consults
//! the registry during activation without loading any plugin, and each plugin
//! artifact links this same crate for activation-side re-validation. Entries
//! are appended as plugin crates land (decision 20: the M1 framework ships
//! with the lookup in place and real schemas attach per crate). The 16c
//! config groups land as two plugin-slug entries: the jobs-local group rides
//! the `fr-shell` row and the goal, schedule, tool-goal, tool-jobs, and
//! tool-todo groups merge into the single `fr-tools-builtin` row (one plugin
//! = one config object; key order follows the dsh config-catalog package
//! order).

use std::sync::OnceLock;

use crate::schema::{Finite, Properties, Property, Schema};

/// Looks up the schema declared for `plugin`, or [`None`] when absent.
///
/// Assembly skips config validation for the rows without a declaration (the
/// dsh `absent` semantics): third-party artifacts have no host-side schema
/// and re-validate inside their own artifact instead.
#[must_use]
pub fn lookup(plugin: &str) -> Option<&'static Schema> {
    table()
        .iter()
        .find(|(name, _)| *name == plugin)
        .map(|(_, schema)| schema)
}

/// The table, built once at first lookup: the `Properties` construction is
/// fallible only against duplicate keys, which the compile-time-known table
/// cannot carry (a duplicate would be a programmer error; the panic text
/// names the violated invariant, never a data-dependent condition).
fn table() -> &'static [(&'static str, Schema)] {
    static TABLE: OnceLock<Vec<(&'static str, Schema)>> = OnceLock::new();
    TABLE.get_or_init(build_table)
}
/// The declarations for the 16c config groups (kebab-case keys, the dsh
/// schemastery constraints ported from the six dsh packages; the 16d
/// interaction keys join the flat builtin row).
#[expect(
    clippy::too_many_lines,
    reason = "the tuple table mirrors the dsh plugin configs one arm per group; splitting it would hide the correspondence"
)]
fn build_table() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "fr-shell",
            object(vec![
                integer_property(
                    "max-concurrent-jobs-per-owner",
                    1,
                    Some(MAX_SAFE_INTEGER),
                    10,
                    "Maximum running plus stopping jobs per owner or in the shared bucket.",
                ),
                integer_property(
                    "retain-bytes",
                    1,
                    Some(MAX_SAFE_INTEGER),
                    262_144,
                    "Live ring retention per job in UTF-8 bytes.",
                ),
                integer_property(
                    "settled-retain-bytes",
                    1,
                    Some(MAX_SAFE_INTEGER),
                    16_384,
                    "Ring retention kept after a job settles, in UTF-8 bytes.",
                ),
                integer_property(
                    "pump-poll-ms",
                    1,
                    Some(MAX_SAFE_INTEGER),
                    150,
                    "Poll interval for a job's pull sources, in milliseconds.",
                ),
                string_property(
                    "backend-type",
                    &[],
                    "shell",
                    "Registry type the local PTY backend registers under.",
                ),
                string_property(
                    "shell-dialect",
                    &["bash", "pwsh"],
                    "bash",
                    "Interactive shell dialect; selects the argv, environment, and startup defaults.",
                ),
                optional_string_property(
                    "shell-path",
                    "Interactive shell executable; empty selects the dialect default.",
                ),
                optional_string_array_property(
                    "shell-args",
                    "Shell argument list; empty selects the dialect default.",
                ),
                number_property("rows", None, None, 40.0, "Terminal rows."),
                number_property("cols", None, None, 160.0, "Terminal columns."),
                number_property(
                    "scrollback-lines",
                    None,
                    None,
                    10_000.0,
                    "Maximum retained logical lines.",
                ),
                number_property(
                    "scrollback-max-bytes",
                    None,
                    None,
                    4_194_304.0,
                    "Maximum retained UTF-8 bytes.",
                ),
                number_property(
                    "max-read-bytes",
                    None,
                    None,
                    262_144.0,
                    "Maximum bytes returned by one read or settled viewport.",
                ),
                number_property(
                    "poll-interval-ms",
                    None,
                    None,
                    50.0,
                    "Readiness polling interval in milliseconds.",
                ),
                number_property(
                    "exact-probe-after-ms",
                    None,
                    None,
                    150.0,
                    "Delay before the exact syscall probes, in milliseconds.",
                ),
                number_property(
                    "idle-silence-ms",
                    None,
                    None,
                    3_000.0,
                    "Silence duration that settles a send as inferred idle, in milliseconds.",
                ),
                number_property(
                    "handoff-grace-ms",
                    None,
                    None,
                    500.0,
                    "Extra wait for the foreground handoff once a prompt marker was seen, in milliseconds.",
                ),
                number_property(
                    "prompt-tail-grace-ms",
                    None,
                    None,
                    0.0,
                    "Extra wait for the prompt tail; zero means no extension.",
                ),
                number_property(
                    "timeout-ms",
                    None,
                    None,
                    30_000.0,
                    "Absolute bound for one send and the complete pwsh startup, in milliseconds.",
                ),
                number_property(
                    "dispose-grace-ms",
                    None,
                    None,
                    3_000.0,
                    "Grace before teardown escalates to SIGKILL, in milliseconds.",
                ),
            ]),
        ),
        (
            "fr-tools-builtin",
            object(vec![
                number_property(
                    "default-max-goal-rounds",
                    None,
                    None,
                    256.0,
                    "Total rounds used when a create request omits its own cap.",
                ),
                integer_property(
                    "delivery-history-days",
                    1,
                    Some(3_650),
                    30,
                    "Delivery-history window retained per task, in days.",
                ),
                integer_property(
                    "delivery-history-records",
                    1,
                    Some(10_000),
                    200,
                    "Retained delivery records per task; the older of the cap and the window wins.",
                ),
                integer_property(
                    "blocked-after-consecutive-rounds",
                    1,
                    None,
                    3,
                    "Minimum admitted goal rounds before the model may self-report blocked.",
                ),
                number_property(
                    "wait-timeout-ms",
                    Some(1.0),
                    None,
                    30_000.0,
                    "Wait duration applied when a wait omits its timeout_ms.",
                ),
                number_property(
                    "max-wait-timeout-ms",
                    Some(1.0),
                    None,
                    600_000.0,
                    "Hard cap on any single wait; a larger model-supplied timeout clamps down to it.",
                ),
                string_property(
                    "completion-delivery",
                    &["quiet", "wakeup"],
                    "wakeup",
                    "How an uncollected completion reaches an idle owner: wakeup opens a turn, quiet leaves it pending.",
                ),
                optional_number_property(
                    "max-consecutive-wakes",
                    "Turns one owner may have opened by completion wakes before notices degrade to injection.",
                ),
                boolean_property(
                    "allow-parallel-in-progress",
                    "Whether several todos may be in_progress at once; false restores single-active discipline.",
                ),
                string_property(
                    "mode",
                    &["legacy", "timed"],
                    "legacy",
                    "Tool definition selected by this Cordis row. Defaults to the blocking legacy tool.",
                ),
                number_property(
                    "timeout",
                    None,
                    None,
                    120.0,
                    "Foreground wait before automatic continuation. Defaults to 120 seconds.",
                ),
                required_string_property(
                    "section",
                    "Guidance rendered as the `plan:policy` prompt section while plan mode is active.",
                ),
                integer_property(
                    "max-search-results",
                    1,
                    None,
                    100,
                    "Maximum authorized hits returned by one session search call.",
                ),
                integer_property(
                    "search-timeout-ms",
                    1,
                    Some(2_147_483_647),
                    30_000,
                    "Cooperative full-text search deadline in milliseconds.",
                ),
                string_property(
                    "journal-mode",
                    &["wal", "delete", "truncate", "persist"],
                    "wal",
                    "SQLite journal mode for the derived search index.",
                ),
                integer_property(
                    "default-limit",
                    1,
                    Some(MAX_SAFE_INTEGER - 1),
                    20,
                    "Page size when a session search request omits limit.",
                ),
                integer_property(
                    "max-limit",
                    1,
                    Some(MAX_SAFE_INTEGER - 1),
                    100,
                    "Largest accepted session search page size.",
                ),
                integer_property(
                    "snippet-chars",
                    1,
                    None,
                    240,
                    "Maximum snippet length in Unicode code points.",
                ),
                integer_property(
                    "persisted-read-concurrency",
                    1,
                    Some(MAX_SAFE_INTEGER),
                    4,
                    "Maximum concurrent persisted-log reads in one inherited batch read.",
                ),
                integer_property(
                    "prepared-session-cache-size",
                    1,
                    Some(MAX_SAFE_INTEGER),
                    5,
                    "Maximum cold prepared observations the inherited reader retains for reuse.",
                ),
                boolean_default_property(
                    "enable-run-in-background",
                    true,
                    "Whether terminal sends accept the background variant that returns a job id.",
                ),
                integer_property(
                    "max-result-bytes",
                    64,
                    Some(MAX_SAFE_INTEGER),
                    262_144,
                    "Maximum UTF-8 bytes in one complete terminal or task-output result.",
                ),
                string_property(
                    "backend-type",
                    &[],
                    "shell",
                    "PTY backend used for each owner-isolated persistent shell (the merged persistent family; the pwsh twin carries the same key).",
                ),
                number_property(
                    "timeout-ms",
                    None,
                    None,
                    300_000.0,
                    "Wall-clock limit for one persistent-shell command, in milliseconds.",
                ),
                number_property(
                    "max-output-chars",
                    None,
                    None,
                    16_000.0,
                    "Maximum persistent-shell command-output characters before clipping.",
                ),
                string_property(
                    "description",
                    &[],
                    "Run commands in a persistent bash shell. State, including the current directory and exported environment variables, persists across calls for this agent.",
                    "Model-facing persistent-shell tool description (the bash twin's text; the pwsh twin defaults to its PowerShell wording).",
                ),
            ]),
        ),
    ]
}

/// The dsh `Number.MAX_SAFE_INTEGER` bound (2^53-1; the jobs-local group's
/// upper bounds).
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Builds one object declaration with unique keys (a duplicate is a
/// programmer error caught at the first lookup).
fn object(properties: Vec<Property>) -> Schema {
    let properties =
        Properties::try_from(properties).expect("the registry declarations carry unique keys");
    Schema::Object {
        properties,
        additional_properties: false,
        description: None,
    }
}

/// One integer property with a range and a default (the dsh
/// `z.number().step(1).min().max().default()` rows).
fn integer_property(
    name: &str,
    min: i64,
    max: Option<i64>,
    default: i64,
    description: &str,
) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Integer {
            min: Some(min),
            max,
            default: Some(default),
            description: Some(description.to_owned()),
        },
    }
}

/// One number property with a range and a default.
fn number_property(
    name: &str,
    min: Option<f64>,
    max: Option<f64>,
    default: f64,
    description: &str,
) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Number {
            min,
            max,
            default: Some(Finite::try_from(default).expect("the registry defaults are finite")),
            description: Some(description.to_owned()),
        },
    }
}

/// One optional number property with a lower bound and no default (the dsh
/// `z.number().min(1)` optional rows).
fn optional_number_property(name: &str, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Number {
            min: Some(1.0),
            max: None,
            default: None,
            description: Some(description.to_owned()),
        },
    }
}

/// One enumeration string property with a default (the dsh
/// `z.union([...]).default()` rows).
fn string_property(name: &str, candidates: &[&str], default: &str, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::String {
            candidates: candidates
                .iter()
                .map(|candidate| (*candidate).to_owned())
                .collect(),
            default: Some(default.to_owned()),
            description: Some(description.to_owned()),
        },
    }
}

/// One free-form string property without a default (the dsh required
/// string rows; the requiredness rides the wrapping `Property.required`).
fn required_string_property(name: &str, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: true,
        schema: Schema::String {
            candidates: Vec::new(),
            default: None,
            description: Some(description.to_owned()),
        },
    }
}

/// One boolean property without a default (the dsh `z.boolean().required()`
/// rows; the requiredness rides the wrapping `Property.required`).
fn boolean_property(name: &str, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: true,
        schema: Schema::Boolean {
            default: None,
            description: Some(description.to_owned()),
        },
    }
}

/// One boolean property with a declared default (the dsh
/// `z.boolean().default()` rows; optional, null falls back to the default).
fn boolean_default_property(name: &str, default: bool, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Boolean {
            default: Some(default),
            description: Some(description.to_owned()),
        },
    }
}

/// One free-form string property without a default (the dsh
/// `z.string().required(false)` rows; absent stays absent).
fn optional_string_property(name: &str, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::String {
            candidates: Vec::new(),
            default: None,
            description: Some(description.to_owned()),
        },
    }
}

/// One optional string-array property (the dsh `z.array(z.string())`
/// optional rows; absent stays absent).
fn optional_string_array_property(name: &str, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Array {
            items: Box::new(Schema::String {
                candidates: Vec::new(),
                default: None,
                description: None,
            }),
            description: Some(description.to_owned()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::lookup;
    use crate::schema::{Schema, to_json_schema, validate};

    fn violations_empty(schema: &Schema, value: &serde_json::Value) -> bool {
        validate(schema, value).is_empty()
    }

    fn first_message(schema: &Schema, value: &serde_json::Value) -> String {
        let violations = validate(schema, value);
        assert!(!violations.is_empty(), "the probe must violate: {value}");
        violations[0].message.clone()
    }

    #[test]
    fn shell_config_pins_jobs_local_group() {
        let schema = lookup("fr-shell").expect("the jobs-local declaration");
        let Schema::Object {
            properties,
            additional_properties,
            ..
        } = schema
        else {
            panic!("the jobs-local declaration is an object");
        };
        assert!(!additional_properties);
        let names: Vec<_> = properties
            .iter()
            .map(|property| property.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "max-concurrent-jobs-per-owner",
                "retain-bytes",
                "settled-retain-bytes",
                "pump-poll-ms",
                "backend-type",
                "shell-dialect",
                "shell-path",
                "shell-args",
                "rows",
                "cols",
                "scrollback-lines",
                "scrollback-max-bytes",
                "max-read-bytes",
                "poll-interval-ms",
                "exact-probe-after-ms",
                "idle-silence-ms",
                "handoff-grace-ms",
                "prompt-tail-grace-ms",
                "timeout-ms",
                "dispose-grace-ms",
            ]
        );
        // Defaults project and an empty config validates against them.
        let projected = to_json_schema(schema);
        let projected_properties = projected
            .pointer("/anyOf/0/properties")
            .expect("projection properties");
        for (key, expected) in [
            ("max-concurrent-jobs-per-owner", 10),
            ("retain-bytes", 262_144),
            ("settled-retain-bytes", 16_384),
        ] {
            let row = projected_properties
                .get(key)
                .unwrap_or_else(|| panic!("projection carries {key}"));
            assert_eq!(row.get("default"), Some(&serde_json::json!(expected)));
        }
        assert!(
            violations_empty(schema, &serde_json::json!({})),
            "an empty config validates against the defaults"
        );
        // The integer face and the bounds reject their probes.
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"max-concurrent-jobs-per-owner": 0})
            ),
            "below minimum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"retain-bytes": 9_007_199_254_740_992i64})
            ),
            "above maximum"
        );
        assert_eq!(
            first_message(schema, &serde_json::json!({"pump-poll-ms": 1.5})),
            "expected integer"
        );
        // The closed face rejects undeclared keys.
        assert_eq!(
            first_message(schema, &serde_json::json!({"unknown": true})),
            "unknown key `unknown`"
        );
    }

    /// The 16e terminal-bash group rides the fr-shell row: the 16 keys
    /// project their dsh defaults, the optional faces validate, and the
    /// closed face rejects undeclared keys.
    #[test]
    fn shell_config_pins_terminal_bash_group() {
        let schema = lookup("fr-shell").expect("the terminal-bash declaration");
        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties")
            .expect("projection properties");
        for (key, expected) in [
            ("backend-type", serde_json::json!("shell")),
            ("shell-dialect", serde_json::json!("bash")),
            ("rows", serde_json::json!(40.0)),
            ("cols", serde_json::json!(160.0)),
            ("scrollback-lines", serde_json::json!(10_000.0)),
            ("scrollback-max-bytes", serde_json::json!(4_194_304.0)),
            ("max-read-bytes", serde_json::json!(262_144.0)),
            ("poll-interval-ms", serde_json::json!(50.0)),
            ("exact-probe-after-ms", serde_json::json!(150.0)),
            ("idle-silence-ms", serde_json::json!(3_000.0)),
            ("handoff-grace-ms", serde_json::json!(500.0)),
            ("prompt-tail-grace-ms", serde_json::json!(0.0)),
            ("timeout-ms", serde_json::json!(30_000.0)),
            ("dispose-grace-ms", serde_json::json!(3_000.0)),
        ] {
            let row = rows
                .get(key)
                .unwrap_or_else(|| panic!("projection carries {key}"));
            assert_eq!(row.get("default"), Some(&expected));
        }
        // The optional faces validate (absent stays absent; present
        // validates).
        assert!(violations_empty(
            schema,
            &serde_json::json!({
                "shell-path": "/bin/zsh",
                "shell-args": ["--norc"],
            })
        ));
        // The closed face rejects undeclared keys.
        assert_eq!(
            first_message(schema, &serde_json::json!({"backend": "shell"})),
            "unknown key `backend`"
        );
    }

    #[test]
    fn builtin_config_pins_todo_switch() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        assert!(
            violations_empty(
                schema,
                &serde_json::json!({"allow-parallel-in-progress": true, "section": "You are in plan mode."})
            ),
            "the true choice validates"
        );
        assert!(
            violations_empty(
                schema,
                &serde_json::json!({"allow-parallel-in-progress": false, "section": "You are in plan mode."})
            ),
            "the false choice validates"
        );
        assert_eq!(
            first_message(schema, &serde_json::json!({})),
            "missing required key `allow-parallel-in-progress`"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"allow-parallel-in-progress": "true"})
            ),
            "expected boolean"
        );
    }

    #[test]
    fn builtin_config_pins_tool_jobs_group() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        // The three catalog-documented facts project their defaults.
        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties")
            .expect("projection properties");
        assert_eq!(
            rows.get("wait-timeout-ms").expect("row").get("default"),
            Some(&serde_json::json!(30_000.0))
        );
        assert_eq!(
            rows.get("max-wait-timeout-ms").expect("row").get("default"),
            Some(&serde_json::json!(600_000.0))
        );
        assert_eq!(
            rows.get("completion-delivery").expect("row").get("default"),
            Some(&serde_json::json!("wakeup"))
        );
        // The wait bounds reject zero; the enum rejects other words; the
        // optional wake cap accepts absence and rejects zero.
        assert_eq!(
            first_message(schema, &serde_json::json!({"wait-timeout-ms": 0})),
            "below minimum"
        );
        assert_eq!(
            first_message(schema, &serde_json::json!({"completion-delivery": "later"})),
            "value not in candidates"
        );
        assert!(
            violations_empty(
                schema,
                &serde_json::json!({"allow-parallel-in-progress": true, "section": "You are in plan mode."})
            ),
            "absence of the optional wake cap validates"
        );
        assert_eq!(
            first_message(schema, &serde_json::json!({"max-consecutive-wakes": 0})),
            "below minimum"
        );
    }

    #[test]
    fn builtin_config_pins_goal_and_threshold_groups() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties")
            .expect("projection properties");
        assert_eq!(
            rows.get("default-max-goal-rounds")
                .expect("row")
                .get("default"),
            Some(&serde_json::json!(256.0))
        );
        assert_eq!(
            rows.get("blocked-after-consecutive-rounds")
                .expect("row")
                .get("default"),
            Some(&serde_json::json!(3))
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"blocked-after-consecutive-rounds": 0})
            ),
            "below minimum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"blocked-after-consecutive-rounds": 1.5})
            ),
            "expected integer"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"default-max-goal-rounds": "many"})
            ),
            "expected number"
        );
    }

    #[test]
    fn builtin_config_pins_delivery_history_group() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties")
            .expect("projection properties");
        assert_eq!(
            rows.get("delivery-history-days")
                .expect("row")
                .get("default"),
            Some(&serde_json::json!(30))
        );
        assert_eq!(
            first_message(schema, &serde_json::json!({"delivery-history-days": 3_651})),
            "above maximum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"delivery-history-records": 10_001})
            ),
            "above maximum"
        );
        assert!(
            violations_empty(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "delivery-history-days": 3_650,
                    "delivery-history-records": 10_000,
                })
            ),
            "the boundary values validate"
        );
    }

    #[test]
    fn builtin_config_pins_session_query_defaults() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties")
            .expect("projection properties");
        for (key, expected) in [
            ("max-search-results", 100),
            ("search-timeout-ms", 30_000),
            ("default-limit", 20),
            ("max-limit", 100),
            ("snippet-chars", 240),
            ("persisted-read-concurrency", 4),
            ("prepared-session-cache-size", 5),
        ] {
            let row = rows
                .get(key)
                .unwrap_or_else(|| panic!("projection carries {key}"));
            assert_eq!(row.get("default"), Some(&serde_json::json!(expected)));
        }
        assert_eq!(
            rows.get("journal-mode").expect("row").get("default"),
            Some(&serde_json::json!("wal"))
        );
    }

    /// The search-bounds and enum probes over the flat row.
    #[test]
    fn builtin_config_pins_session_query_probes() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "max-search-results": 0,
                })
            ),
            "below minimum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "search-timeout-ms": 0,
                })
            ),
            "below minimum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "search-timeout-ms": 2_147_483_648i64,
                })
            ),
            "above maximum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "journal-mode": "memory",
                })
            ),
            "value not in candidates"
        );
        assert!(
            violations_empty(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "max-search-results": 1,
                    "search-timeout-ms": 30_000,
                    "journal-mode": "wal",
                    "default-limit": 1,
                    "max-limit": 100,
                    "snippet-chars": 240,
                    "persisted-read-concurrency": 4,
                    "prepared-session-cache-size": 5,
                })
            ),
            "the boundary values validate"
        );
    }

    #[test]
    fn builtin_config_pins_session_query_boundaries() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "max-limit": 9_007_199_254_740_992i64,
                })
            ),
            "above maximum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "prepared-session-cache-size": 9_007_199_254_740_992i64,
                })
            ),
            "above maximum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "default-limit": 1.5,
                })
            ),
            "expected integer"
        );
    }

    /// The dsh-tool-ask-user and dsh-plan-mode rows plus the two
    /// session-read bound probes the earlier session-query tests leave
    /// uncovered (the 16d batch-3 binding surface).
    #[test]
    fn builtin_config_pins_ask_user_plan_mode_and_read_bounds() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties")
            .expect("projection properties");
        assert_eq!(
            rows.get("mode").expect("row").get("default"),
            Some(&serde_json::json!("legacy"))
        );
        assert_eq!(
            rows.get("timeout").expect("row").get("default"),
            Some(&serde_json::json!(120.0))
        );
        assert!(
            rows.get("section").is_some(),
            "the plan-mode section row exists"
        );
        // The mode enum refuses unknown selections; the timeout row is
        // a plain number (the dsh config-catalog row; the timed
        // -1|1..2147483 union rides the tool body validation).
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "mode": "fast",
                })
            ),
            "value not in candidates"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "timeout": "120s",
                })
            ),
            "expected number"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                })
            ),
            "missing required key `section`"
        );
        // The two session-read bound probes: snippet chars floor and
        // the persisted read concurrency band.
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "snippet-chars": 0,
                })
            ),
            "below minimum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "persisted-read-concurrency": 0,
                })
            ),
            "below minimum"
        );
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({
                    "allow-parallel-in-progress": true,
                    "section": "You are in plan mode.",
                    "persisted-read-concurrency": 9_007_199_254_740_992i64,
                })
            ),
            "above maximum"
        );
    }

    /// The 16e tool-terminal and merged persistent family ride the
    /// fr-tools-builtin row: defaults project and the closed face
    /// rejects undeclared keys.
    #[test]
    fn builtin_config_pins_terminal_tool_group() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties")
            .expect("projection properties");
        for (key, expected) in [
            ("enable-run-in-background", serde_json::json!(true)),
            ("max-result-bytes", serde_json::json!(262_144)),
            ("backend-type", serde_json::json!("shell")),
            ("timeout-ms", serde_json::json!(300_000.0)),
            ("max-output-chars", serde_json::json!(16_000.0)),
            (
                "description",
                serde_json::json!(
                    "Run commands in a persistent bash shell. State, including the current directory and exported environment variables, persists across calls for this agent."
                ),
            ),
        ] {
            let row = rows
                .get(key)
                .unwrap_or_else(|| panic!("projection carries {key}"));
            assert_eq!(row.get("default"), Some(&expected));
        }
        assert!(violations_empty(
            schema,
            &serde_json::json!({"allow-parallel-in-progress": true, "section": "p", "backend-type": "pty-x", "timeout-ms": 1000.0, "max-output-chars": 100.0, "description": "x"})
        ));
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"allow-parallel-in-progress": true, "section": "p", "max-result-bytes": 63})
            ),
            "below minimum"
        );
    }
}
