//! Fly Rafter host binary (`fr`).
//!
//! This slice lands the dump CLI surface (PRD 23 decision 27, Q30B): the
//! `dump-config` / `dump-config-schema` subcommands plus the root invocation
//! face. Argument semantics mirror dsh `apps/cli/src/args.ts` verbatim where
//! a dsh counterpart exists; the assembler, event pump, and entry dispatch
//! arrive with the full M1 host slice (PRD 20 Phase 18).

#![forbid(unsafe_code)]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use fr_config::dump::collect_config_schema;
use fr_config::{
    DumpLayer, EntryKind, EvalContext, LoadedProfile, ROOT_FILENAME, collect_dump_layers,
    prepare_profile, render_dump, schema_lookup,
};

const USAGE: &str = "Usage:\n  fr dump-config [--profile <name>] [--from-default-profile <name>] [--patch <path>]... [--default-only]\n  fr dump-config-schema [--profile <name>] [--from-default-profile <name>] [--patch <path>]...\n  fr -V, --version    output the version number\n  fr -h, --help       show this help";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(mode) = argv.first() else {
        return usage_error();
    };
    match mode.as_str() {
        "dump-config" => run_dump(&argv[1..], false),
        "dump-config-schema" => run_dump(&argv[1..], true),
        "-V" | "--version" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "-h" | "--help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => usage_error(),
    }
}

fn usage_error() -> ExitCode {
    eprintln!("error: expected a mode (dump-config | dump-config-schema)");
    eprintln!("{USAGE}");
    ExitCode::from(1)
}

/// One resolved dump invocation (the fr shape of dsh's
/// `DumpConfigInvocation` / `DumpConfigSchemaInvocation`).
struct DumpInvocation {
    profile: String,
    from_default_profile: Option<String>,
    default_only: bool,
    patches: Vec<PathBuf>,
}

/// Value of an option: the inline `=value` part, else the next argv entry;
/// an option at the end of argv is missing its value entirely.
fn option_value(
    inline: Option<String>,
    argv: &[String],
    index: &mut usize,
    missing: &str,
) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    *index += 1;
    argv.get(*index).cloned().ok_or_else(|| missing.to_owned())
}

/// Parses dump subcommand arguments with the dsh launcher's split semantics:
/// the first token the parser does not own starts the inner arguments, and
/// everything from there on belongs to the booted app verbatim (including
/// its own flags and `-h`).
fn parse_dump_args(argv: &[String], schema_mode: bool) -> Result<DumpInvocation, String> {
    let mut profile: Option<String> = None;
    let mut from_default_profile: Option<String> = None;
    let mut default_only = false;
    let mut patches: Vec<PathBuf> = Vec::new();
    let mut inner: Vec<String> = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let (flag, inline) = match argv[index].split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (argv[index].as_str(), None),
        };
        match flag {
            "--profile" => {
                if profile.is_some() {
                    return Err("error: select a profile only once".to_owned());
                }
                profile = Some(option_value(
                    inline,
                    argv,
                    &mut index,
                    "error: --profile needs a name",
                )?);
            }
            "--from-default-profile" => {
                from_default_profile = Some(option_value(
                    inline,
                    argv,
                    &mut index,
                    "error: --from-default-profile needs a name",
                )?);
            }
            "--patch" => {
                let value = option_value(inline, argv, &mut index, "error: --patch needs a path")?;
                patches.push(PathBuf::from(value));
            }
            "--default-only" if !schema_mode && inline.is_none() => default_only = true,
            _ => {
                inner.extend_from_slice(&argv[index..]);
                break;
            }
        }
        index += 1;
    }
    // Validation order mirrors dsh `resolveBoot`: the profile first, then the
    // flag values, then the inner arguments, then the default-only split.
    let Some(profile) = profile else {
        return Err("error: --profile <name> is required".to_owned());
    };
    if profile.is_empty() {
        return Err("error: --profile needs a name".to_owned());
    }
    if patches.iter().any(|path| path.as_os_str().is_empty()) {
        return Err("error: --patch needs a path".to_owned());
    }
    if from_default_profile.as_ref().is_some_and(String::is_empty) {
        return Err("error: --from-default-profile needs a name".to_owned());
    }
    if !inner.is_empty() {
        let listed = inner
            .iter()
            .map(|argument| {
                serde_json::to_string(argument)
                    .expect("serde_json cannot fail to serialize a string")
            })
            .collect::<Vec<_>>()
            .join(" ");
        return Err(format!(
            "error: config dumps take no app arguments, got {listed}"
        ));
    }
    if default_only && !patches.is_empty() {
        return Err(
            "error: --default-only prints the bundle layers and takes no --patch".to_owned(),
        );
    }
    Ok(DumpInvocation {
        profile,
        from_default_profile,
        default_only,
        patches,
    })
}

fn run_dump(argv: &[String], schema_mode: bool) -> ExitCode {
    let invocation = match parse_dump_args(argv, schema_mode) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(1);
        }
    };
    match dump(&invocation, schema_mode) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn dump(invocation: &DumpInvocation, schema_mode: bool) -> fr_config::error::Result<ExitCode> {
    let home = fr_config::resolve_home(None)
        .map_err(|error| fr_config::Error::Io(format!("cannot resolve the fr home: {error}")))?;
    let ctx = EvalContext::process(true);
    let warn = |message: &str| eprintln!("{message}");
    // The schema dump always assembles with the user layer; dump-config drops
    // it only for --default-only (dsh dump-config-schema.ts / profile-boot).
    let user_layer = schema_mode || !invocation.default_only;
    let loaded = prepare_profile(
        &home,
        &invocation.profile,
        user_layer,
        invocation.from_default_profile.as_deref(),
        &warn,
        &ctx,
    )?;
    // M1 ships a single entry, so the dump's entry bundle layer is headless
    // (decision 27); the entry selection face arrives with M2 entries.
    let layers = collect_dump_layers(
        &home,
        EntryKind::Headless,
        &loaded,
        invocation.default_only,
        &invocation.patches,
        "fr",
    )?;
    if schema_mode {
        return dump_schema(&loaded, &layers);
    }
    let root = loaded.dir.join(ROOT_FILENAME);
    let text = render_dump("fr", &root, &layers, &warn)?;
    print!("{text}");
    Ok(ExitCode::SUCCESS)
}

/// Prints one pretty JSON document plus a newline to stdout, mirrors each
/// diagnostic to stderr (`fr: <level>: [<path>] <message>`), and sets the
/// exit code from completeness (dsh `runDumpConfigSchema`).
fn dump_schema(loaded: &LoadedProfile, layers: &[DumpLayer]) -> fr_config::error::Result<ExitCode> {
    let dumped = collect_config_schema(loaded, layers, &schema_lookup);
    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "{}",
        serde_json::to_string_pretty(&dumped.document).expect("schema documents are JSON values")
    )
    .map_err(|error| fr_config::Error::Io(format!("failed to write stdout: {error}")))?;
    for diagnostic in &dumped.diagnostics {
        let location = diagnostic
            .path
            .as_ref()
            .map_or(String::new(), |path| format!(" [{path}]"));
        eprintln!(
            "fr: {}:{} {}",
            diagnostic.level.as_str(),
            location,
            diagnostic.message
        );
    }
    Ok(if dumped.complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
