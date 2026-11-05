//! The headless exit watcher (退出守望, the runner's exit request lane).
//!
//! The runner requests process exit through the `headless/exit` `{code}`
//! kernel event (the 18b ruling: the plugin-to-host push face is emit;
//! the host cannot provide a plugin-side service, and require/dispatch
//! ride the vtable from plugin-owned threads only). A host-embedded
//! in-process plugin listens for the event, decodes the code, and hands
//! it to the serve loop through a channel; the serve loop races it
//! against the signal face. Registration happens in the body per the
//! in-process contract, and the kernel collects the listener as an
//! effect (LIFO reversal at teardown).

use std::sync::Arc;

use dougong::{BoxFuture, Context, Next, plugin};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

/// The exit request event (the 18b 退出轨 ruling): payload `{"code": N}`.
pub const EVENT_HEADLESS_EXIT: &str = "headless/exit";

/// The receiver half of the exit lane: the serve loop races it against
/// the signal wait.
#[derive(Debug)]
pub struct ExitRequest {
    rx: UnboundedReceiver<u32>,
}

impl ExitRequest {
    /// Receives the runner's exit code; `None` when the watcher fiber is
    /// gone (the serve loop keeps waiting for a signal in that case — the
    /// runner path is dead, so a signal is the only exit left).
    pub async fn recv(&mut self) -> Option<u32> {
        self.rx.recv().await
    }

    /// A dead lane (both halves dropped): the serve loop detaches the
    /// tree's lane before its select, so the placeholder never resolves.
    #[must_use]
    pub fn dead() -> Self {
        Self {
            rx: tokio::sync::mpsc::unbounded_channel().1,
        }
    }
}

/// The event dispatcher face: decode `{"code": N}` and forward on the
/// channel. Exposed for the pin test.
///
/// # Errors
///
/// Returns the decode refusal for a missing or out-of-range code.
pub fn serve_event(payload: &Value, tx: &Arc<ChannelFace>) -> Result<(), String> {
    let Some(code) = payload.get("code").and_then(Value::as_u64) else {
        return Err("headless/exit payload requires a code".to_owned());
    };
    let code =
        u32::try_from(code).map_err(|error| format!("headless/exit code exceeds u32: {error}"));
    match code {
        Ok(code) => {
            let _ = tx.send(code);
            Ok(())
        }
        Err(message) => Err(message),
    }
}

/// The channel face (test double surface: the pin test records the codes
/// the watcher forwards).
pub type ChannelFace = tokio::sync::mpsc::UnboundedSender<u32>;

/// Registers the exit-watcher plugin on the boot context and hands back
/// the receive lane.
///
/// Registered before the facts relay (the runner spawns at the relay, so
/// the lane must exist first) and joined within the boot bound.
///
/// # Errors
///
/// Fails closed when the fiber cannot start or does not activate within
/// the deadline.
pub async fn spawn_watcher(
    ctx: &Context,
    deadline: tokio::time::Instant,
) -> Result<ExitRequest, crate::diagnostics::BootFatal> {
    let fatal = |summary: String| crate::diagnostics::BootFatal {
        detail: summary.clone(),
        summary,
    };
    let (tx, rx) = unbounded_channel();
    let tx = Arc::new(tx);
    let watcher = plugin(
        "fr-exit-watcher",
        vec![],
        move |ctx: Context, _config: Value| {
            let tx = Arc::clone(&tx);
            Box::pin(async move {
                ctx.on(
                    EVENT_HEADLESS_EXIT,
                    Arc::new(move |_ctx: Context, payload: Value, _next: Next| {
                        let tx = Arc::clone(&tx);
                        Box::pin(async move {
                            let _ = serve_event(&payload, &tx);
                            Ok(payload)
                        }) as BoxFuture<dougong::Result<Value>>
                    }),
                )
                .await
                .map_err(|error| {
                    dougong::Error::new(
                        "exit_watcher_listener_failed",
                        format!("exit watcher listener: {error}"),
                    )
                })?;
                Ok(())
            })
        },
    );
    let handle = ctx
        .plugin(watcher, None)
        .map_err(|error| fatal(format!("fr: exit watcher registration failed: {error}")))?;
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    handle
        .join_with_timeout(remaining)
        .await
        .map_err(|error| fatal(format!("fr: exit watcher did not activate: {error}")))?;
    Ok(ExitRequest { rx })
}

#[cfg(test)]
mod tests {
    use super::{EVENT_HEADLESS_EXIT, ExitRequest, serve_event};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn exit_event_name_is_pinned() {
        assert_eq!(EVENT_HEADLESS_EXIT, "headless/exit");
    }

    #[tokio::test]
    async fn exit_event_forwards_the_code_on_the_lane() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        serve_event(&json!({ "code": 7 }), &Arc::new(tx)).expect("the code decodes");
        assert_eq!(rx.recv().await, Some(7));
    }

    #[test]
    fn exit_event_without_a_code_fails_closed() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = serve_event(&json!({}), &Arc::new(tx)).expect_err("no code");
        assert!(error.contains("requires a code"), "{error}");
    }

    #[test]
    fn exit_event_beyond_u32_fails_closed() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = serve_event(&json!({ "code": 4_294_967_296_u64 }), &Arc::new(tx))
            .expect_err("overflow");
        assert!(error.contains("exceeds u32"), "{error}");
    }

    #[tokio::test]
    async fn dead_lane_reports_no_code() {
        let mut lane = ExitRequest::dead();
        assert_eq!(lane.recv().await, None);
    }
}
