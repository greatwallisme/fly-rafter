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
                group_property(
                    "web",
                    "Execution-time provider pins for the web seam; each read applies to the next search or fetch.",
                    vec![
                        optional_string_property(
                            "search-provider",
                            "Explicit search provider id; omitted = auto-select when exactly one usable.",
                        ),
                        optional_string_property(
                            "fetch-provider",
                            "Explicit fetch provider id; omitted = auto-select when exactly one usable.",
                        ),
                    ],
                ),
                group_property(
                    "tool-web",
                    "Which web tools to register, search bounds, per-tool budgets, and the fetch output cap.",
                    vec![
                        boolean_default_property("search", true, "Register `web_search`."),
                        boolean_default_property("fetch", true, "Register `web_fetch`."),
                        number_property(
                            "search-max-results",
                            None,
                            None,
                            8.0,
                            "Upper bound on sources returned by one `web_search` call.",
                        ),
                        number_property(
                            "search-max-queries",
                            None,
                            None,
                            4.0,
                            "Upper bound on queries accepted by one `web_search` call.",
                        ),
                        number_property(
                            "fetch-timeout-ms",
                            None,
                            None,
                            30_000.0,
                            "Cooperative timeout budget (ms) for `web_fetch`.",
                        ),
                        number_property(
                            "search-timeout-ms",
                            None,
                            None,
                            30_000.0,
                            "Cooperative timeout budget (ms) for `web_search`.",
                        ),
                        number_property(
                            "fetch-max-output-chars",
                            None,
                            None,
                            200_000.0,
                            "Cap on source characters converted and complete `web_fetch` output characters.",
                        ),
                    ],
                ),
                group_property(
                    "web-deepseek-api",
                    "The DeepSeek Search and Fetch API providers' shared credential and request settings.",
                    vec![
                        optional_string_property(
                            "api-key",
                            "Literal DeepSeek API key; prefer `api-key-env` so no secret enters configuration files.",
                        ),
                        string_property(
                            "api-key-env",
                            &[],
                            "DEEPSEEK_API_KEY",
                            "Credential reference resolved for each operation.",
                        ),
                        string_property(
                            "base-url",
                            &[],
                            "https://api.deepseek.com/v1/tools",
                            "API base including its path prefix; `/search` and `/fetch` are appended.",
                        ),
                        integer_property(
                            "count",
                            1,
                            Some(50),
                            10,
                            "Search API results per search, from 1 through 50; a smaller request bound wins.",
                        ),
                        boolean_default_property(
                            "send-original-question",
                            true,
                            "Send the triggering user question, bounded to 128 characters, as `ori_question`.",
                        ),
                    ],
                ),
                group_property(
                    "web-fetch-http",
                    "The local HTTP fetch provider's transport and size limits plus its user agent.",
                    vec![
                        number_property(
                            "max-response-bytes",
                            None,
                            None,
                            5_000_000.0,
                            "Maximum response body size in bytes.",
                        ),
                        number_property(
                            "max-body-chars",
                            None,
                            None,
                            100_000.0,
                            "Maximum decoded body length in characters.",
                        ),
                        number_property(
                            "timeout-ms",
                            None,
                            None,
                            30_000.0,
                            "Default fetch timeout in milliseconds.",
                        ),
                        number_property(
                            "max-redirects",
                            None,
                            None,
                            5.0,
                            "Maximum number of same-origin redirect hops to follow.",
                        ),
                        string_property(
                            "user-agent",
                            &[],
                            "fly-rafter/0.1.0 (+https://github.com/greatwallisme/fly-rafter)",
                            "User-Agent header sent on every request; the live default rides the product attribution's version.",
                        ),
                    ],
                ),
                group_property(
                    "web-search-deepseek",
                    "The DeepSeek Anthropic-format search provider's credential, endpoint, and request bounds.",
                    vec![
                        optional_string_property(
                            "api-key",
                            "Literal DeepSeek API key; prefer `api-key-env` so no secret enters configuration files.",
                        ),
                        string_property(
                            "api-key-env",
                            &[],
                            "DEEPSEEK_API_KEY",
                            "Credential reference resolved for each search.",
                        ),
                        optional_string_property(
                            "base-url",
                            "Anthropic-compatible endpoint base; `/messages` is appended.",
                        ),
                        string_property(
                            "model",
                            &[],
                            "deepseek-v4-flash",
                            "Anthropic-format model name.",
                        ),
                        string_property(
                            "api-version",
                            &[],
                            "2023-06-01",
                            "`anthropic-version` header value.",
                        ),
                        integer_property(
                            "max-tokens",
                            1,
                            None,
                            4_096,
                            "Upper bound on generated tokens for the Messages request.",
                        ),
                        integer_property(
                            "max-uses",
                            1,
                            None,
                            5,
                            "Maximum `web_search` server-tool uses per request.",
                        ),
                    ],
                ),
                group_property(
                    "web-search-exa",
                    "The Exa search provider's credential and request defaults.",
                    vec![
                        optional_string_property(
                            "api-key",
                            "Exa API key; falls back to the `EXA_API_KEY` environment variable; empty leaves the provider unavailable.",
                        ),
                        optional_string_property(
                            "base-url",
                            "Endpoint base; `/search` is appended.",
                        ),
                        optional_enum_property(
                            "search-type",
                            &["auto", "keyword", "neural"],
                            "Retrieval mode sent as Exa's `type`.",
                        ),
                        optional_integer_property(
                            "num-results",
                            1,
                            "Default result count when a request carries no `maxResults`.",
                        ),
                        optional_integer_property(
                            "highlights-per-result",
                            1,
                            "Highlight sentences requested per result.",
                        ),
                    ],
                ),
                group_property(
                    "web-search-perplexity",
                    "The Perplexity search provider's credential and request defaults.",
                    vec![
                        optional_string_property(
                            "api-key",
                            "Perplexity API key; falls back to the `PERPLEXITY_API_KEY` environment variable; empty leaves the provider unavailable.",
                        ),
                        optional_string_property(
                            "base-url",
                            "Endpoint base; `/chat/completions` is appended.",
                        ),
                        optional_string_property("model", "Search model name."),
                        optional_integer_property(
                            "max-tokens",
                            1,
                            "Upper bound on generated answer tokens.",
                        ),
                        optional_enum_property(
                            "search-recency",
                            &["day", "week", "month", "year"],
                            "Recency window sent as `search_recency_filter`.",
                        ),
                    ],
                ),
                group_property(
                    "tool-lsp",
                    "The lsp tool's result caps and its tool-call timeout budget.",
                    vec![
                        number_property(
                            "max-locations",
                            None,
                            None,
                            100.0,
                            "Largest number of rendered locations before an omission marker.",
                        ),
                        number_property(
                            "max-result-chars",
                            None,
                            None,
                            16_000.0,
                            "Largest complete rendered result in characters.",
                        ),
                        number_property(
                            "timeout-ms",
                            None,
                            Some(2_147_483_647.0),
                            60_000.0,
                            "Tool-call timeout budget in ms.",
                        ),
                    ],
                ),
                group_property(
                    "skill",
                    "The skill registry's completed-catalog cache capacity.",
                    vec![integer_property(
                        "collect-cache-max-entries",
                        1,
                        Some(MAX_SAFE_INTEGER),
                        128,
                        "Maximum number of completed cwd/provider catalogs kept in memory.",
                    )],
                ),
                group_property(
                    "skill-filesystem",
                    "The local filesystem skill provider's roots, watch behavior, and bundled root.",
                    vec![
                        string_property(
                            "provider-name",
                            &[],
                            "filesystem",
                            "Unique provider name.",
                        ),
                        boolean_default_property(
                            "include-default-roots",
                            true,
                            "Whether project and user roots are included around custom roots.",
                        ),
                        optional_string_property(
                            "fr-home",
                            "Harness config root; defaults to `$FR_HOME` or `~/.fr`.",
                        ),
                        optional_string_property(
                            "agents-home",
                            "Shared agent config root; defaults to `$FR_AGENTS_HOME` or `~/.agents`.",
                        ),
                        optional_string_array_property(
                            "custom-skill-dirs",
                            "Additional skill roots scanned after project roots and before user roots; empty selects none.",
                        ),
                        boolean_default_property(
                            "watch",
                            true,
                            "Whether host-local skill roots are watched for catalog changes.",
                        ),
                        boolean_default_property(
                            "watch-use-polling",
                            false,
                            "Whether the watcher uses polling instead of native filesystem events.",
                        ),
                        integer_property(
                            "watch-stability-threshold-ms",
                            1,
                            Some(MAX_SAFE_INTEGER),
                            200,
                            "Milliseconds a changed skill entry must remain stable before it is observed.",
                        ),
                        integer_property(
                            "watch-poll-interval-ms",
                            1,
                            Some(MAX_SAFE_INTEGER),
                            100,
                            "Milliseconds between stability or polling probes.",
                        ),
                        integer_property(
                            "watch-max-projects",
                            1,
                            Some(MAX_SAFE_INTEGER),
                            128,
                            "Maximum distinct project roots whose skill directories remain watched.",
                        ),
                        boolean_default_property(
                            "watch-follow-symlinks",
                            true,
                            "Whether watched symbolic links follow their target files.",
                        ),
                        optional_string_property(
                            "bundled-skill-dir",
                            "Bundled skill root; defaults to `$FR_BUNDLED_SKILL_DIR` when default roots are included, otherwise mounts none.",
                        ),
                    ],
                ),
                group_property(
                    "tool-skill",
                    "The model-facing skill catalog's description budget.",
                    vec![integer_property(
                        "catalog-description-max-length",
                        3,
                        Some(MAX_SAFE_INTEGER),
                        500,
                        "Maximum normalized description length rendered in the session catalog; minimum 3.",
                    )],
                ),
            ]),
        ),
        ("fr-compaction", compaction_row()),
    ]
}

/// The `fr-compaction` declaration: the dsh compaction-basic schemastery
/// face (`packages/compaction/compaction-basic/src/index.ts:81-128`) with
/// kebab-case keys — the nine policy fields top-level, the exact-target
/// policy table, and the automatic-listener switch.
fn compaction_row() -> Schema {
    let mut model_policy_properties = vec![
        required_string_property("provider", "Registered provider route to match."),
        required_string_property("model", "Exact routed model id to match within provider."),
    ];
    model_policy_properties.extend(compaction_policy_properties());
    let mut properties = compaction_policy_properties();
    properties.push(Property {
        name: "model-policies".to_owned(),
        required: false,
        schema: Schema::Array {
            items: Box::new(object(model_policy_properties)),
            description: Some(
                "Exact provider/model overrides; duplicate targets fail plugin load.".to_owned(),
            ),
        },
    });
    properties.push(optional_boolean_property(
        "auto",
        "Enable automatic step-boundary pressure and overflow-recovery listeners. Defaults to true.",
    ));
    object(properties)
}

/// The nine compaction policy fields shared by the top-level defaults and
/// each exact-target override (the dsh `CompactionPolicyConfig` fields; the
/// schemastery declares no defaults — resolution owns them).
fn compaction_policy_properties() -> Vec<Property> {
    vec![
        optional_unbounded_number_property(
            "threshold-ratio",
            "Window fraction for pressure; capped at context window minus reserved output and headroom tokens. Defaults to 0.8.",
        ),
        optional_integer_property(
            "headroom-tokens",
            0,
            "Additional pressure headroom beyond the routed output reservation. Non-negative integer; defaults to 65536.",
        ),
        optional_unbounded_number_property(
            "retain-ratio",
            "Recent context retained as a fraction of context window minus reserved output tokens. Defaults to 0.16.",
        ),
        optional_integer_property(
            "retain-tokens",
            0,
            "Absolute recent-context budget; mutually exclusive with retain-ratio.",
        ),
        optional_string_property(
            "summarization-provider",
            "Summary provider; set together with summarization-model, or inherit the conversation target.",
        ),
        optional_string_property(
            "summarization-model",
            "Summary model; set together with summarization-provider, or inherit the conversation target.",
        ),
        optional_integer_property(
            "max-tokens",
            1,
            "Provider generation cap for summarization. Defaults to the resolved headroom; an explicit cap must be positive.",
        ),
        optional_integer_property(
            "compaction-retries",
            0,
            "Extra attempts after the first compaction when pressure remains above threshold. Defaults to 1.",
        ),
        optional_integer_property(
            "max-overflow-retries",
            0,
            "Maximum retries after canonical context overflow; 0 disables recovery. Defaults to 1.",
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

/// One optional number property without bounds or default (the dsh
/// `z.number()` raw rows; range discipline belongs to resolution).
fn optional_unbounded_number_property(name: &str, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Number {
            min: None,
            max: None,
            default: None,
            description: Some(description.to_owned()),
        },
    }
}

/// One optional boolean property without a default (the dsh `z.boolean()`
/// raw rows).
fn optional_boolean_property(name: &str, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Boolean {
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

/// One optional enumeration string property without a default (the dsh
/// `z.union([...])` optional rows).
fn optional_enum_property(name: &str, candidates: &[&str], description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::String {
            candidates: candidates
                .iter()
                .map(|candidate| (*candidate).to_owned())
                .collect(),
            default: None,
            description: Some(description.to_owned()),
        },
    }
}

/// One optional integer property with a lower bound and no default (the
/// dsh `z.number().step(1).min(1)` optional rows).
fn optional_integer_property(name: &str, min: i64, description: &str) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Integer {
            min: Some(min),
            max: None,
            default: None,
            description: Some(description.to_owned()),
        },
    }
}

/// One nested config-group property (the 16f ruling: the new web
/// package groups ride as sub-objects while the 16c-16e keys stay
/// flat).
fn group_property(name: &str, description: &str, properties: Vec<Property>) -> Property {
    Property {
        name: name.to_owned(),
        required: false,
        schema: Schema::Object {
            properties: Properties::try_from(properties)
                .expect("the registry declarations carry unique keys"),
            additional_properties: false,
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

    /// The 16f web groups ride as the seven nested package sections
    /// (the nested-groups ruling: the new groups nest while the
    /// 16c-16e keys stay flat). The defaults are the dsh schema
    /// defaults, never the deployed bakes — the search budget's
    /// schema default stays 30000 while the deployed 60000 rides the
    /// baked constant.
    #[test]
    fn builtin_config_pins_web_groups() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let Schema::Object { properties, .. } = schema else {
            panic!("the builtin-tools declaration is an object");
        };
        let group = |name: &str| {
            let property = properties
                .iter()
                .find(|property| property.name == name)
                .unwrap_or_else(|| panic!("the {name} group declaration"));
            let Schema::Object {
                properties,
                additional_properties,
                ..
            } = &property.schema
            else {
                panic!("the {name} group is an object");
            };
            assert!(!additional_properties, "{name} refuses unknown keys");
            properties
                .iter()
                .map(|property| (property.name.as_str(), &property.schema))
                .collect::<Vec<_>>()
        };
        let web = group("web");
        assert_eq!(
            web.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec!["search-provider", "fetch-provider"]
        );
        let tool_web = group("tool-web");
        let tool_web_names: Vec<_> = tool_web.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            tool_web_names,
            vec![
                "search",
                "fetch",
                "search-max-results",
                "search-max-queries",
                "fetch-timeout-ms",
                "search-timeout-ms",
                "fetch-max-output-chars",
            ]
        );
        for key in [
            "search-max-results",
            "search-max-queries",
            "fetch-timeout-ms",
            "search-timeout-ms",
            "fetch-max-output-chars",
        ] {
            let row = tool_web
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, schema)| schema)
                .expect(key);
            assert!(
                matches!(
                    row,
                    Schema::Number {
                        default: Some(_),
                        ..
                    }
                ),
                "{key} is a defaulted number row"
            );
        }
        let api = group("web-deepseek-api");
        assert_eq!(
            api.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec![
                "api-key",
                "api-key-env",
                "base-url",
                "count",
                "send-original-question",
            ]
        );
        let count = api
            .iter()
            .find(|(name, _)| *name == "count")
            .map(|(_, schema)| schema)
            .expect("count");
        let Schema::Integer {
            min, max, default, ..
        } = count
        else {
            panic!("count is an integer row");
        };
        assert_eq!((min, max), (&Some(1), &Some(50)));
        assert_eq!(default, &Some(10));
    }

    /// The four provider groups ride their dsh package sections with
    /// their declared key sets (the nested-groups ruling; the enum
    /// candidates verbatim).
    #[test]
    fn builtin_config_pins_tool_lsp_group() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let Schema::Object { properties, .. } = schema else {
            panic!("the builtin-tools declaration is an object");
        };
        let property = properties
            .iter()
            .find(|property| property.name == "tool-lsp")
            .expect("the tool-lsp group declaration");
        let Schema::Object {
            properties: rows,
            additional_properties,
            ..
        } = &property.schema
        else {
            panic!("the tool-lsp group is an object");
        };
        assert!(!additional_properties, "tool-lsp refuses unknown keys");
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["max-locations", "max-result-chars", "timeout-ms"]
        );
        for row in rows.iter() {
            assert!(
                matches!(
                    &row.schema,
                    Schema::Number {
                        default: Some(_),
                        ..
                    }
                ),
                "{} is a defaulted number row",
                row.name
            );
        }
        let projected = to_json_schema(schema);
        let group_rows = projected
            .pointer("/anyOf/0/properties/tool-lsp/anyOf/0/properties")
            .expect("nested tool-lsp projection");
        assert_eq!(
            group_rows
                .get("max-locations")
                .and_then(|row| row.get("default")),
            Some(&serde_json::json!(100.0))
        );
        assert_eq!(
            group_rows
                .get("timeout-ms")
                .and_then(|row| row.get("default")),
            Some(&serde_json::json!(60000.0))
        );
        assert_eq!(
            group_rows
                .get("max-result-chars")
                .and_then(|row| row.get("default")),
            Some(&serde_json::json!(16000.0))
        );
        // The timeout budget rides the timer bound (the dsh
        // `MAX_TIMER_DELAY_MS` cap).
        let row = rows
            .iter()
            .find(|row| row.name == "timeout-ms")
            .expect("timeout row");
        let Schema::Number { max, .. } = &row.schema else {
            panic!("timeout-ms is a number row");
        };
        assert_eq!(*max, Some(2_147_483_647.0));
    }

    #[test]
    fn builtin_config_pins_web_provider_groups() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let Schema::Object { properties, .. } = schema else {
            panic!("the builtin-tools declaration is an object");
        };
        let group = |name: &str| {
            let property = properties
                .iter()
                .find(|property| property.name == name)
                .unwrap_or_else(|| panic!("the {name} group declaration"));
            let Schema::Object {
                properties,
                additional_properties,
                ..
            } = &property.schema
            else {
                panic!("the {name} group is an object");
            };
            assert!(!additional_properties, "{name} refuses unknown keys");
            properties
                .iter()
                .map(|property| (property.name.as_str(), &property.schema))
                .collect::<Vec<_>>()
        };
        let official = group("web-search-deepseek");
        assert_eq!(
            official.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec![
                "api-key",
                "api-key-env",
                "base-url",
                "model",
                "api-version",
                "max-tokens",
                "max-uses",
            ]
        );
        let http = group("web-fetch-http");
        assert_eq!(
            http.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec![
                "max-response-bytes",
                "max-body-chars",
                "timeout-ms",
                "max-redirects",
                "user-agent",
            ]
        );
        let exa = group("web-search-exa");
        assert_eq!(
            exa.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec![
                "api-key",
                "base-url",
                "search-type",
                "num-results",
                "highlights-per-result",
            ]
        );
        let search_type = exa
            .iter()
            .find(|(name, _)| *name == "search-type")
            .map(|(_, schema)| schema)
            .expect("search-type");
        let Schema::String { candidates, .. } = search_type else {
            panic!("search-type is a string row");
        };
        assert_eq!(
            candidates,
            &["auto".to_owned(), "keyword".to_owned(), "neural".to_owned()]
        );
        let perplexity = group("web-search-perplexity");
        assert_eq!(
            perplexity.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec![
                "api-key",
                "base-url",
                "model",
                "max-tokens",
                "search-recency",
            ]
        );
    }

    /// The dump-config projection carries the nested groups one level
    /// down, with the schema defaults riding the group rows.
    #[test]
    fn builtin_config_projects_web_groups() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties/tool-web/anyOf/0/properties")
            .expect("nested tool-web projection");
        assert_eq!(
            rows.get("search-timeout-ms").expect("row").get("default"),
            Some(&serde_json::json!(30_000.0))
        );
        let api_rows = projected
            .pointer("/anyOf/0/properties/web-deepseek-api/anyOf/0/properties")
            .expect("nested api projection");
        assert_eq!(
            api_rows.get("count").expect("row").get("default"),
            Some(&serde_json::json!(10))
        );
        assert_eq!(
            api_rows.get("base-url").expect("row").get("default"),
            Some(&serde_json::json!("https://api.deepseek.com/v1/tools"))
        );
    }

    /// The nested-group probes: the enum and range constraints refuse
    /// inside the sub-objects, unknown nested keys refuse, and the
    /// flat legacy keys stay untouched alongside.
    #[test]
    fn builtin_config_rejects_bad_web_group_values() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let out_of_range = validate(
            schema,
            &serde_json::json!({
                "allow-parallel-in-progress": true,
                "section": "You are in plan mode.",
                "web-deepseek-api": { "count": 51 },
            }),
        );
        assert!(
            out_of_range
                .iter()
                .any(|violation| violation.path.contains("count")),
            "{out_of_range:?}"
        );
        let bad_enum = validate(
            schema,
            &serde_json::json!({
                "allow-parallel-in-progress": true,
                "section": "You are in plan mode.",
                "web-search-perplexity": { "search-recency": "hour" },
            }),
        );
        assert!(
            bad_enum
                .iter()
                .any(|violation| violation.path.contains("search-recency")),
            "{bad_enum:?}"
        );
        let unknown_key = validate(
            schema,
            &serde_json::json!({
                "allow-parallel-in-progress": true,
                "section": "You are in plan mode.",
                "web": { "searchProvider": "exa" },
            }),
        );
        assert!(
            unknown_key
                .iter()
                .any(|violation| violation.message.contains("unknown key")),
            "{unknown_key:?}"
        );
        // The search budget accepts the deployed override shape.
        let accepted = validate(
            schema,
            &serde_json::json!({
                "allow-parallel-in-progress": true,
                "section": "You are in plan mode.",
                "tool-web": { "search-timeout-ms": 60_000.0 },
            }),
        );
        assert!(accepted.is_empty(), "{accepted:?}");
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
    /// The 16f batch-2 skill family: the three groups pin their dsh
    /// key sets, defaults, and range bounds (the tool-lsp judgment),
    /// and the range bounds refuse inside the nested objects.
    #[expect(
        clippy::too_many_lines,
        reason = "the pin mirrors the dsh skill-family configs one arm per group; splitting it would hide the correspondence"
    )]
    #[test]
    fn builtin_config_pins_skill_groups() {
        let schema = lookup("fr-tools-builtin").expect("the builtin-tools declaration");
        let Schema::Object { properties, .. } = schema else {
            panic!("the builtin-tools declaration is an object");
        };
        let skill = properties
            .iter()
            .find(|property| property.name == "skill")
            .expect("the skill group declaration");
        let Schema::Object {
            properties: rows,
            additional_properties,
            ..
        } = &skill.schema
        else {
            panic!("the skill group is an object");
        };
        assert!(
            !additional_properties,
            "the skill group refuses unknown keys"
        );
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["collect-cache-max-entries"]);
        let Schema::Integer {
            min, max, default, ..
        } = &rows[0].schema
        else {
            panic!("collect-cache-max-entries is an integer row");
        };
        assert_eq!(
            (min, max, default),
            (&Some(1), &Some(super::MAX_SAFE_INTEGER), &Some(128))
        );

        let filesystem = properties
            .iter()
            .find(|property| property.name == "skill-filesystem")
            .expect("the skill-filesystem group declaration");
        let Schema::Object {
            properties: rows,
            additional_properties,
            ..
        } = &filesystem.schema
        else {
            panic!("the skill-filesystem group is an object");
        };
        assert!(
            !additional_properties,
            "the skill-filesystem group refuses unknown keys"
        );
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "provider-name",
                "include-default-roots",
                "fr-home",
                "agents-home",
                "custom-skill-dirs",
                "watch",
                "watch-use-polling",
                "watch-stability-threshold-ms",
                "watch-poll-interval-ms",
                "watch-max-projects",
                "watch-follow-symlinks",
                "bundled-skill-dir",
            ]
        );
        let defaults: Vec<_> = rows
            .iter()
            .map(|row| match &row.schema {
                Schema::Integer { default, .. } => default.map(serde_json::Value::from),
                Schema::Number { default, .. } => default.map(serde_json::Value::from),
                Schema::Boolean { default, .. } => default.map(serde_json::Value::from),
                Schema::String { default, .. } => default.clone().map(serde_json::Value::from),
                _ => None,
            })
            .collect();
        assert_eq!(
            defaults,
            [
                Some(serde_json::json!("filesystem")),
                Some(serde_json::json!(true)),
                None,
                None,
                None,
                Some(serde_json::json!(true)),
                Some(serde_json::json!(false)),
                Some(serde_json::json!(200)),
                Some(serde_json::json!(100)),
                Some(serde_json::json!(128)),
                Some(serde_json::json!(true)),
                None,
            ]
        );

        let tool_skill = properties
            .iter()
            .find(|property| property.name == "tool-skill")
            .expect("the tool-skill group declaration");
        let Schema::Object {
            properties: rows,
            additional_properties,
            ..
        } = &tool_skill.schema
        else {
            panic!("the tool-skill group is an object");
        };
        assert!(
            !additional_properties,
            "the tool-skill group refuses unknown keys"
        );
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["catalog-description-max-length"]);
        let Schema::Integer { min, default, .. } = &rows[0].schema else {
            panic!("catalog-description-max-length is an integer row");
        };
        assert_eq!((min, default), (&Some(3), &Some(500)));

        let projected = to_json_schema(schema);
        let rows = projected
            .pointer("/anyOf/0/properties/tool-skill/anyOf/0/properties")
            .expect("nested tool-skill projection");
        assert_eq!(
            rows.get("catalog-description-max-length")
                .expect("row")
                .get("default"),
            Some(&serde_json::json!(500))
        );

        // The range bounds refuse inside the nested objects (the
        // web-groups judgment).
        let below_minimum = validate(
            schema,
            &serde_json::json!({
                "skill": { "collect-cache-max-entries": 0 },
            }),
        );
        assert!(
            below_minimum
                .iter()
                .any(|violation| violation.path.contains("collect-cache-max-entries")),
            "{below_minimum:?}"
        );
        let below_catalog_floor = validate(
            schema,
            &serde_json::json!({
                "tool-skill": { "catalog-description-max-length": 2 },
            }),
        );
        assert!(
            below_catalog_floor
                .iter()
                .any(|violation| violation.path.contains("catalog-description-max-length")),
            "{below_catalog_floor:?}"
        );
        let below_watch_floor = validate(
            schema,
            &serde_json::json!({
                "skill-filesystem": { "watch-stability-threshold-ms": 0 },
            }),
        );
        assert!(
            below_watch_floor
                .iter()
                .any(|violation| violation.path.contains("watch-stability-threshold-ms")),
            "{below_watch_floor:?}"
        );
        let unknown_nested_key = validate(
            schema,
            &serde_json::json!({
                "skill-filesystem": { "watchMaxProjects": 4 },
            }),
        );
        assert!(
            !unknown_nested_key.is_empty(),
            "the camelCase dsh key is not an fr key"
        );
    }

    /// The fr-compaction row pins the dsh compaction-basic schemastery
    /// face: eleven optional top-level keys, the required provider/model
    /// identity pair inside each policy entry, and the closed object faces.
    #[test]
    fn compaction_config_pins_the_policy_table() {
        let schema = lookup("fr-compaction").expect("the compaction declaration");
        let Schema::Object {
            properties,
            additional_properties,
            ..
        } = schema
        else {
            panic!("the compaction declaration is an object");
        };
        assert!(!additional_properties);
        let names: Vec<_> = properties
            .iter()
            .map(|property| property.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "threshold-ratio",
                "headroom-tokens",
                "retain-ratio",
                "retain-tokens",
                "summarization-provider",
                "summarization-model",
                "max-tokens",
                "compaction-retries",
                "max-overflow-retries",
                "model-policies",
                "auto",
            ]
        );
        // Every key is optional; the defaults live in resolution.
        assert!(
            violations_empty(schema, &serde_json::json!({})),
            "an empty config validates"
        );
        // The closed face rejects undeclared keys.
        assert_eq!(
            first_message(schema, &serde_json::json!({"models": {}})),
            "unknown key `models`"
        );
        // The integer and number faces reject their probes.
        assert_eq!(
            first_message(schema, &serde_json::json!({"headroom-tokens": -1})),
            "below minimum"
        );
        assert_eq!(
            first_message(schema, &serde_json::json!({"max-tokens": 0})),
            "below minimum"
        );
        assert_eq!(
            first_message(schema, &serde_json::json!({"compaction-retries": 1.5})),
            "expected integer"
        );
        // Each policy entry carries the required identity pair.
        assert!(violations_empty(
            schema,
            &serde_json::json!({"model-policies": [{"provider": "p", "model": "m"}]})
        ));
        assert_eq!(
            first_message(
                schema,
                &serde_json::json!({"model-policies": [{"model": "m"}]})
            ),
            "missing required key `provider`"
        );
        let below_headroom_floor = validate(
            schema,
            &serde_json::json!({"model-policies": [{"provider": "p", "model": "m", "headroom-tokens": -1}]}),
        );
        assert!(
            below_headroom_floor
                .iter()
                .any(|violation| violation.path.contains("headroom-tokens")),
            "{below_headroom_floor:?}"
        );
        // The nested policy object projects the closed face.
        let projected = to_json_schema(schema);
        assert_eq!(
            projected.pointer(
                "/anyOf/0/properties/model-policies/anyOf/0/items/anyOf/0/\
                 additionalProperties"
            ),
            Some(&serde_json::json!(false))
        );
    }
}
