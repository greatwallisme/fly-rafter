//! End-to-end proof for the fr-session dylib artifact: the cdylib loads
//! over the dougong loader, a consumer binds the provided service through
//! the dynamic channel, and the vocabulary declaration surface behaves
//! (PRD 7.1 Q34A, PRD 4.3 dylib form).

use std::path::PathBuf;
use std::process::Command;

use dougong::{Context, Injection, Invoker, plugin};
use dougong_dylib::load;

use fr_session::plugin_shell::{METHOD_DECLARE_EVENT_TYPES, METHOD_DESCRIBE};

/// Builds the cdylib artifact (a no-op when the workspace is current) and
/// returns its path.
fn build_and_locate_artifact() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "fr-session"])
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .expect("run cargo build");
    assert!(status.success(), "cargo build -p fr-session failed");
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.extend(["..", "..", "target", "debug"]);
    let name = if cfg!(target_os = "macos") {
        "libfr_session.dylib"
    } else if cfg!(windows) {
        "fr_session.dll"
    } else {
        "libfr_session.so"
    };
    path.join(name)
}

#[test]
fn dylib_artifact_loads_and_serves_the_vocabulary_service() {
    let path = build_and_locate_artifact();
    let fr_session_plugin: std::sync::Arc<dyn dougong::Plugin> =
        load(&path).expect("load the fr-session dylib");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    runtime.block_on(async move {
        let ctx = Context::new();

        // The consumer declares the service before the provider exists: it
        // parks in Pending and rewires when the dylib fiber provides.
        let consumer = plugin(
            "vocabulary-consumer",
            vec![Injection::new("fr-session")],
            move |ctx, _config| {
                Box::pin(async move {
                    let invoker = ctx.require::<Invoker>("fr-session")?;

                    // First declare: two fresh plugin-owned types land.
                    let reply = invoker
                        .call(
                            METHOD_DECLARE_EVENT_TYPES,
                            serde_json::json!({
                                "types": ["compaction/start", "compaction/end"]
                            })
                            .to_string()
                            .into_bytes(),
                        )
                        .await?;
                    let reply: serde_json::Value = serde_json::from_slice(&reply)
                        .map_err(|error| dougong::Error::msg(error.to_string()))?;
                    assert_eq!(reply, serde_json::json!({ "declared": 2 }));

                    // Duplicate declaration fails closed (assembly error).
                    let duplicate = invoker
                        .call(
                            METHOD_DECLARE_EVENT_TYPES,
                            serde_json::json!({ "types": ["compaction/start"] })
                                .to_string()
                                .into_bytes(),
                        )
                        .await;
                    assert!(duplicate.is_err(), "duplicate declaration must fail closed");

                    // Describe reflects the aggregate (14 core + 2 declared).
                    let reply = invoker.call(METHOD_DESCRIBE, Vec::new()).await?;
                    let reply: serde_json::Value = serde_json::from_slice(&reply)
                        .map_err(|error| dougong::Error::msg(error.to_string()))?;
                    assert_eq!(reply["name"], "fr-session");
                    assert_eq!(reply["formatVersion"], 4);
                    assert_eq!(reply["knownTypes"], 16);
                    Ok(())
                })
            },
        );

        let consumer_fiber = ctx.plugin(consumer, None).expect("spawn consumer");
        let provider_fiber = ctx.plugin(fr_session_plugin, None).expect("spawn provider");
        provider_fiber.join().await.expect("provider fiber");
        consumer_fiber.join().await.expect("consumer fiber");
    });
}

// Rust guideline compliant 2026-10-23
