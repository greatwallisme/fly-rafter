//! The host event pump (事件泵).
//!
//! The kernel's event queue is the dispatch machinery (fixed worker pool,
//! per-event-name FIFO); the pump face adds the host-owned relay duty
//! fr-session's Q43-A drain lane documents: the pump drains the outbound
//! mirror lane and re-emits each record onto the kernel bus as
//! `session/*` events, payload verbatim. Plugin-to-plugin delivery rides
//! the kernel dispatch (descriptor listen tables) with no host
//! involvement; the pump's own duty is the fr-session drain relay
//! (fr-session service.rs:30-33 contract).

use dougong::{Context, Invoker};
use serde_json::Value;
use tokio::time::interval;

/// The pump tick: the drain cadence. The lane is advisory (fr-session
/// Q43-A), so a bounded tick trades latency for a quiescent idle loop; the
/// agent loop's own notifications wake the loop side, not the pump.
const PUMP_TICK: std::time::Duration = std::time::Duration::from_millis(50);

/// Spawns the host pump task on the boot runtime.
#[must_use]
pub fn spawn(ctx: Context) -> tokio::task::JoinHandle<()> {
    tokio::spawn(pump_loop(ctx))
}

/// The pump loop: drain → re-emit, one tick at a time.
async fn pump_loop(ctx: Context) {
    let mut tick = interval(PUMP_TICK);
    loop {
        tick.tick().await;
        let Ok(invoker) = ctx.require::<Invoker>("session-persistence") else {
            continue;
        };
        let Ok(reply) = invoker.call(8, b"{}".to_vec()).await else {
            continue;
        };
        let Ok(envelopes) = serde_json::from_slice::<Value>(&reply) else {
            continue;
        };
        for record in envelopes
            .get("records")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(kind) = record.get("kind").and_then(Value::as_str) else {
                continue;
            };
            // Re-emit verbatim: the record payload crosses unchanged (the
            // fr-session Q43-A contract names the pump as the relay).
            ctx.emit(
                &format!("session/{kind}"),
                record.get("payload").cloned().unwrap_or(Value::Null),
            );
        }
    }
}
