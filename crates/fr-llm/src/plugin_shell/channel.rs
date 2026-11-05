//! The channel-side generation proxy (the runtime-registry slice, 18b
//! Q7A): one [`ChannelSource`] per registered provider route streams the
//! provider plugin's own generation face over the dynamic channel.
//!
//! Shape enforcement (the mechanics that forced this design): an
//! `Arc<dyn GenerationSource>` cannot cross the C-ABI boundary, a dylib
//! activation receives no slot config (so the provider-to-service mapping
//! cannot be host-baked), and a provider dispatch arm must never block —
//! the dylib instance lock spans the whole dispatch, so a long-poll
//! `drain` would starve `abort`. The proxy therefore rides the Q84-B2
//! notification twin: the provider face appends to its own hub and emits
//! `llm/provider-chunk-ready`; the fr-llm shell's `on_event` fast path
//! raises a one-bit wake (never a `require`); the proxy worker pulls with
//! `drain` and parks between batches. Abort travels on the hub-minted
//! signal's [`crate::adapter::AbortSignal::on_abort`] callback rail —
//! event-driven, no polling.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use dougong_abi::HostExports;
use dougong_abi::STATUS_CAPACITY;
use dougong_abi::STATUS_OK;
use futures_core::Stream;

use crate::adapter::GenerateOptions;
use crate::adapter::ImageRequestPrice;
use crate::error::LlmError;
use crate::error::TRANSPORT_CODE;
use crate::generation::AbortRequest;
use crate::generation::BeginReply;
use crate::generation::BeginRequest;
use crate::generation::DrainReply;
use crate::generation::DrainRequest;
use crate::generation::GenerationSource;
use crate::generation::METHOD_ABORT;
use crate::generation::METHOD_BEGIN;
use crate::generation::METHOD_DRAIN;
use crate::generation::METHOD_IMAGE_REQUEST_PRICING;
use crate::generation::METHOD_PREPARE_CALL;
use crate::generation::PrepareCallReply;
use crate::generation::PrepareCallRequest;
use crate::generation::from_wire_call_config;
use crate::generation::from_wire_retry_policy;
use crate::generation::to_wire_call_config;

/// Upper bound for one cross-plugin reply (the shared ceiling).
const MAX_REPLY_CAPACITY: usize = 16 * 1024 * 1024;

/// The synchronous require face over one copied `HostExports` vtable
/// (the copied-vtable contract as the emit seam; callable from any
/// plugin thread until `teardown` returns).
#[derive(Debug, Clone, Copy)]
pub(super) struct RequireLink {
    host: HostExports,
}

// SAFETY: the ABI vtable contract states every entry is callable from any
// plugin thread, concurrently; the copy stays valid until this activation's
// `teardown` returns.
unsafe impl Send for RequireLink {}
// SAFETY: sharing one copy only issues concurrent vtable calls, which the
// same contract explicitly allows.
unsafe impl Sync for RequireLink {}

impl RequireLink {
    /// Wraps a vtable copied at `setup`.
    pub(crate) const fn new(host: HostExports) -> Self {
        Self { host }
    }

    /// Performs one synchronous cross-plugin call (growable buffer
    /// protocol; `STATUS_CAPACITY` doubles up to the shared ceiling).
    /// A non-OK status returns the status and the failure body verbatim
    /// so the caller can classify the face's own document.
    pub(super) fn call(
        &self,
        service: &str,
        method: u32,
        payload: &[u8],
    ) -> Result<Vec<u8>, (i32, String)> {
        let mut cap = 4096_usize;
        loop {
            let mut buf = vec![0_u8; cap];
            let mut len = 0_usize;
            // SAFETY: the copied vtable is live until this activation's
            // `teardown` returns; buffers are caller-owned per the
            // export-table contract.
            let status = unsafe {
                (self.host.require)(
                    self.host.host_handle,
                    service.as_ptr(),
                    service.len(),
                    method,
                    payload.as_ptr(),
                    payload.len(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut len,
                )
            };
            if status == STATUS_CAPACITY {
                if cap >= MAX_REPLY_CAPACITY {
                    return Err((
                        status,
                        format!("reply still exceeds {MAX_REPLY_CAPACITY} bytes"),
                    ));
                }
                cap = cap.saturating_mul(2);
                continue;
            }
            buf.truncate(len);
            if status == STATUS_OK {
                return Ok(buf);
            }
            return Err((
                status,
                format!(
                    "require {service}.{method} failed with host status {status}: {}",
                    String::from_utf8_lossy(&buf)
                ),
            ));
        }
    }
}

/// The one-bit proxy wake: `ready` marks buffered face output, `aborted`
/// marks the caller-abort relay. Both flags are sticky between checks —
/// the Q84-B2 one-bit semantics where duplication and loss are harmless.
#[derive(Debug, Default)]
pub(super) struct ProxyWake {
    state: Mutex<WakeState>,
    signal: std::sync::Condvar,
}

#[derive(Debug, Default)]
struct WakeState {
    ready: bool,
    aborted: bool,
}

impl ProxyWake {
    fn new() -> Self {
        Self::default()
    }

    /// Raises the ready bit and wakes the parked worker.
    fn mark_ready(&self) {
        {
            let mut state = self.state.lock().expect("proxy wake lock poisoned");
            state.ready = true;
        }
        self.signal.notify_all();
    }

    /// Raises the aborted bit and wakes the parked worker.
    fn mark_aborted(&self) {
        {
            let mut state = self.state.lock().expect("proxy wake lock poisoned");
            state.aborted = true;
        }
        self.signal.notify_all();
    }

    /// Parks until a bit is raised; clears `ready` on return. Returns
    /// whether the abort bit was raised.
    fn wait(&self) -> bool {
        let mut state = self.state.lock().expect("proxy wake lock poisoned");
        while !state.ready && !state.aborted {
            state = self.signal.wait(state).expect("proxy wake lock poisoned");
        }
        state.ready = false;
        state.aborted
    }
}

/// The per-shell channel registry: the copied require link every proxy
/// shares, one wake per in-flight face generation, and the proxy worker
/// handles the shell joins at teardown.
#[derive(Default)]
pub(super) struct ChannelRegistry {
    link: Mutex<Option<RequireLink>>,
    wakes: Mutex<HashMap<String, Arc<ProxyWake>>>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl std::fmt::Debug for ChannelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let wakes = self.wakes.lock().map_or(0, |wakes| wakes.len());
        let workers = self.workers.lock().map_or(0, |workers| workers.len());
        f.debug_struct("ChannelRegistry")
            .field("linked", &self.link.lock().is_ok_and(|link| link.is_some()))
            .field("wakes", &wakes)
            .field("workers", &workers)
            .finish()
    }
}

impl ChannelRegistry {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Installs the copied require link (the shell's `setup` vtable).
    pub(super) fn install_link(&self, host: HostExports) {
        *self.link.lock().expect("channel registry lock poisoned") = Some(RequireLink::new(host));
    }

    /// Raises the ready bit of one face generation (the `on_event` fast
    /// path: payload parse and this map probe only — never a `require`).
    pub(super) fn mark_ready(&self, generation_id: &str) {
        let wake = {
            let wakes = self.wakes.lock().expect("channel registry lock poisoned");
            wakes.get(generation_id).map(std::sync::Arc::clone)
        };
        if let Some(wake) = wake {
            wake.mark_ready();
        }
    }

    /// Registers the wake for one face generation; the worker calls this
    /// before its first `drain`, which closes the missed-notification
    /// race (any event fired before registration corresponds to output
    /// already buffered on the face, which the unconditional first drain
    /// picks up).
    fn register_wake(&self, generation_id: &str, wake: Arc<ProxyWake>) {
        self.wakes
            .lock()
            .expect("channel registry lock poisoned")
            .insert(generation_id.to_owned(), wake);
    }

    fn remove_wake(&self, generation_id: &str) {
        self.wakes
            .lock()
            .expect("channel registry lock poisoned")
            .remove(generation_id);
    }

    fn push_worker(&self, worker: std::thread::JoinHandle<()>) {
        self.workers
            .lock()
            .expect("channel registry lock poisoned")
            .push(worker);
    }

    /// Joins every proxy worker (the shell's teardown, after the
    /// generation hub's `cancel_all` flipped the abort relays and before
    /// `shutdown` joins the hub workers: a proxy exits only after
    /// forwarding the face's terminal settlement, so the hub workers
    /// converge once every proxy is gone).
    pub(super) fn join_workers(&self) {
        let handles = self
            .workers
            .lock()
            .expect("channel registry lock poisoned")
            .drain(..)
            .collect::<Vec<_>>();
        for handle in handles {
            // A panicked proxy already stopped forwarding; the join result
            // is diagnostic only.
            drop(handle.join());
        }
    }

    fn require(&self, service: &str, method: u32, payload: &[u8]) -> Result<Vec<u8>, LlmError> {
        let link = *self.link.lock().expect("channel registry lock poisoned");
        let Some(link) = link else {
            return Err(LlmError::try_new(
                "the llm channel has no host link (the shell booted without a host)",
                TRANSPORT_CODE,
                None,
                None,
                None,
                None,
            )
            .expect("the static no-link rejection is impossible"));
        };
        match link.call(service, method, payload) {
            Ok(reply) => Ok(reply),
            Err((_status, text)) => Err(require_failure(&text)),
        }
    }
}

/// Classifies one failed face call: the face's own `llmError` document
/// keeps its code and message (the loop's error-document classification),
/// everything else lands as a transport failure.
fn require_failure(text: &str) -> LlmError {
    // The link's failure text embeds the body after the status; recover a
    // `{"llmError": ...}` document when the body carries one.
    let Some(body_start) = text.find("{\"llmError\"") else {
        return LlmError::try_new(text, TRANSPORT_CODE, None, None, None, None)
            .expect("the static transport rejection is impossible");
    };
    let body = &text[body_start..];
    let Ok(document) = serde_json::from_str::<serde_json::Value>(body) else {
        return LlmError::try_new(text, TRANSPORT_CODE, None, None, None, None)
            .expect("the static transport rejection is impossible");
    };
    let Some(failure) = document.get("llmError") else {
        return LlmError::try_new(text, TRANSPORT_CODE, None, None, None, None)
            .expect("the static transport rejection is impossible");
    };
    let message = failure
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(text);
    let code = failure
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(TRANSPORT_CODE);
    LlmError::try_new(message, code, None, None, None, None)
        .expect("the recovered face failure is non-empty")
}

/// The [`GenerationSource`] over one provider plugin's generation-face
/// service. `prepare` round-trips the face's `prepare_call`; `stream`
/// spawns the proxy worker and returns the receiving stream immediately
/// (the source contract: memory-level, the worker owns the blocking
/// require calls).
pub(super) struct ChannelSource {
    /// The provider plugin's generation-face service name.
    service: String,
    registry: Arc<ChannelRegistry>,
}

impl std::fmt::Debug for ChannelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSource")
            .field("service", &self.service)
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl ChannelSource {
    pub(super) const fn new(service: String, registry: Arc<ChannelRegistry>) -> Self {
        Self { service, registry }
    }

    fn begin(&self, request: &GenerateOptions) -> Result<String, LlmError> {
        let payload = BeginRequest {
            request: request.clone().into(),
            prepared_id: None,
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| transport_text_error(&format!("channel begin encode: {error}")))?;
        let reply = self.registry.require(&self.service, METHOD_BEGIN, &bytes)?;
        let reply: BeginReply = serde_json::from_slice(&reply)
            .map_err(|error| transport_text_error(&format!("channel begin reply: {error}")))?;
        Ok(reply.generation_id)
    }

    fn drain(&self, generation_id: &str) -> Result<DrainReply, LlmError> {
        let payload = DrainRequest {
            generation_id: generation_id.to_owned(),
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| transport_text_error(&format!("channel drain encode: {error}")))?;
        let reply = self.registry.require(&self.service, METHOD_DRAIN, &bytes)?;
        serde_json::from_slice(&reply)
            .map_err(|error| transport_text_error(&format!("channel drain reply: {error}")))
    }

    fn abort(&self, generation_id: &str) -> Result<(), LlmError> {
        let payload = AbortRequest {
            generation_id: generation_id.to_owned(),
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| transport_text_error(&format!("channel abort encode: {error}")))?;
        self.registry.require(&self.service, METHOD_ABORT, &bytes)?;
        Ok(())
    }
}

fn transport_text_error(message: &str) -> LlmError {
    LlmError::try_new(message, TRANSPORT_CODE, None, None, None, None)
        .expect("the static transport rejection is impossible")
}

impl GenerationSource for ChannelSource {
    fn prepare(
        &self,
        config: &crate::adapter::LlmCallConfig,
    ) -> Result<
        (
            crate::generation::PreparedRecord,
            crate::adapter::LlmCallConfig,
        ),
        LlmError,
    > {
        let payload = PrepareCallRequest {
            config: to_wire_call_config(config),
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| transport_text_error(&format!("channel prepare encode: {error}")))?;
        let reply = self
            .registry
            .require(&self.service, METHOD_PREPARE_CALL, &bytes)?;
        let reply: PrepareCallReply = serde_json::from_slice(&reply)
            .map_err(|error| transport_text_error(&format!("channel prepare reply: {error}")))?;
        let record = crate::generation::PreparedRecord {
            model: reply.model.into(),
            adapter_defaults: reply.adapter_defaults,
            retry_policy: reply.retry_policy.as_ref().map(from_wire_retry_policy),
        };
        Ok((record, from_wire_call_config(&reply.config)))
    }

    fn stream(
        &self,
        options: GenerateOptions,
    ) -> Pin<Box<dyn Stream<Item = fr_wire::StreamChunk> + Send>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let waker: WakerSlot = Arc::new(Mutex::new(None));
        let forwarder = Forwarder {
            sender,
            waker: Arc::clone(&waker),
        };
        let wake = Arc::new(ProxyWake::new());
        let registry = Arc::clone(&self.registry);
        let worker_registry = Arc::clone(&registry);
        let service = self.service.clone();
        // The abort relay: the hub-minted signal's callback rail raises
        // the wake's aborted bit — event-driven, no polling.
        if let Some(signal) = options.signal.clone() {
            let relay = Arc::clone(&wake);
            signal.on_abort(Arc::new(move || relay.mark_aborted()));
        }
        let worker = std::thread::Builder::new()
            .name("fr-llm-channel-proxy".to_owned())
            .spawn(move || proxy_run(&worker_registry, service, &wake, &options, forwarder));
        if let Ok(handle) = worker {
            registry.push_worker(handle);
        }
        // A failed spawn dropped the closure together with the forwarder
        // it captured: the channel is closed, so the stream ends and the
        // hub settles its FIFO on the stream-end arm.
        Box::pin(ProxyStream { receiver, waker })
    }

    fn image_request_pricing(
        &self,
        provider: &str,
        model: &str,
    ) -> Option<Arc<dyn crate::adapter::ImageRequestPricing>> {
        Some(Arc::new(ChannelPricing {
            service: self.service.clone(),
            registry: Arc::clone(&self.registry),
            provider: provider.to_owned(),
            model: model.to_owned(),
        }))
    }
}

/// The pricing object over one face route: prices resolve per call
/// through the channel (the object itself cannot cross the boundary).
struct ChannelPricing {
    service: String,
    registry: Arc<ChannelRegistry>,
    provider: String,
    model: String,
}

impl crate::adapter::ImageRequestPricing for ChannelPricing {
    fn price_images(&self, images: &[fr_wire::ContentBlock]) -> Vec<ImageRequestPrice> {
        let payload = serde_json::json!({
            "provider": self.provider,
            "model": self.model,
            "images": images,
        });
        let Ok(bytes) = serde_json::to_vec(&payload) else {
            return Vec::new();
        };
        let Ok(reply) = self
            .registry
            .require(&self.service, METHOD_IMAGE_REQUEST_PRICING, &bytes)
        else {
            // The pricing face has no error channel; a failed round trip
            // prices nothing (the meter keeps its fixed heuristic).
            return Vec::new();
        };
        let Ok(document) = serde_json::from_slice::<serde_json::Value>(&reply) else {
            return Vec::new();
        };
        document
            .get("prices")
            .and_then(serde_json::Value::as_array)
            .map(|prices| {
                prices
                    .iter()
                    .filter_map(|price| {
                        Some(ImageRequestPrice {
                            visual_tokens: price.get("visualTokens")?.as_u64()?,
                            text: price
                                .get("text")
                                .and_then(serde_json::Value::as_str)?
                                .to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The receiving half of one proxy run: an async stream over the sync
/// channel, with the classic register-then-recheck waker discipline. The
/// worker holds the same waker slot and wakes it after every send (the
/// sender side of the registration contract).
struct ProxyStream {
    receiver: std::sync::mpsc::Receiver<fr_wire::StreamChunk>,
    waker: WakerSlot,
}

/// The waker slot shared between the receiving stream and the proxy
/// worker's forward relay.
type WakerSlot = Arc<Mutex<Option<Waker>>>;

/// The worker-side forward relay: every delivered chunk wakes the parked
/// consumer (the mpsc channel alone carries no wakeup).
struct Forwarder {
    sender: std::sync::mpsc::Sender<fr_wire::StreamChunk>,
    waker: WakerSlot,
}

impl Forwarder {
    /// Delivers one chunk; `false` when the consumer is gone.
    fn forward(&self, chunk: fr_wire::StreamChunk) -> bool {
        if self.sender.send(chunk).is_err() {
            return false;
        }
        let waker = self
            .waker
            .lock()
            .expect("proxy stream waker lock poisoned")
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    /// Closes the channel and wakes the consumer one final time.
    ///
    /// The order is the contract: the sender drops first, so a poll that
    /// runs after the wake observes the disconnect, and a poll that
    /// registered its waker earlier is woken by the take below. Waking
    /// before the close lets the woken consumer re-poll against a still
    /// live sender, park again, and never be woken by the later drop.
    fn finish(self) {
        let Self { sender, waker } = self;
        drop(sender);
        let waker = waker
            .lock()
            .expect("proxy stream waker lock poisoned")
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl Stream for ProxyStream {
    type Item = fr_wire::StreamChunk;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.receiver.try_recv() {
            Ok(chunk) => Poll::Ready(Some(chunk)),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Poll::Ready(None),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                *this.waker.lock().expect("proxy stream waker lock poisoned") =
                    Some(cx.waker().clone());
                // The sender may have delivered between the probe and the
                // registration; re-check before parking.
                match this.receiver.try_recv() {
                    Ok(chunk) => Poll::Ready(Some(chunk)),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Poll::Ready(None),
                    Err(std::sync::mpsc::TryRecvError::Empty) => Poll::Pending,
                }
            }
        }
    }
}

/// Builds the loop-facing terminal error settlement for a proxy run that
/// cannot reach the face (the runtime's `fr-error` consumption mapping
/// classifies it the same as an adapter-held error).
fn terminal_error_chunk(message: &str) -> fr_wire::StreamChunk {
    let failure = LlmError::try_new(message, TRANSPORT_CODE, None, None, None, None)
        .expect("the static terminal rejection is impossible")
        .failure()
        .clone();
    fr_wire::StreamChunk::Known(fr_wire::stream::KnownStreamChunk::Finish {
        reason: fr_wire::call_config::FinishReason::Known(
            fr_wire::call_config::FinishReasonKind::Error { failure },
        ),
        replay_state: None,
    })
}

/// One proxy run: begin the face generation, drain it to the terminal
/// settlement, forward every chunk. The run owns the blocking require
/// calls; the wake bit paces the drain loop (never a sleep).
fn proxy_run(
    registry: &Arc<ChannelRegistry>,
    service: String,
    wake: &Arc<ProxyWake>,
    options: &GenerateOptions,
    forwarder: Forwarder,
) {
    let source = ChannelSource {
        service,
        registry: Arc::clone(registry),
    };
    let generation_id = match source.begin(options) {
        Ok(generation_id) => generation_id,
        Err(error) => {
            let _ = forwarder.forward(terminal_error_chunk(&format!(
                "channel begin failed: {}",
                error.failure().message
            )));
            forwarder.finish();
            return;
        }
    };
    registry.register_wake(&generation_id, Arc::clone(wake));
    let mut abort_relayed = false;
    let mut consumer_gone = false;
    loop {
        match source.drain(&generation_id) {
            Ok(reply) => {
                for chunk in reply.chunks {
                    if !forwarder.forward(chunk) {
                        // The consumer dropped the stream: relay the abort
                        // and leave the face settlement to the face's own
                        // teardown (dsh caller-abort semantics).
                        consumer_gone = true;
                        break;
                    }
                }
                if reply.done || consumer_gone {
                    break;
                }
            }
            Err(error) => {
                let _ = forwarder.forward(terminal_error_chunk(&format!(
                    "channel drain failed: {}",
                    error.failure().message
                )));
                break;
            }
        }
        if wake.wait() && !abort_relayed {
            // The abort relay fired (possibly together with a ready bit):
            // relay once, then keep draining — the face settles the
            // caller-abort arm with its own terminal, which the loop
            // forwards before exiting.
            abort_relayed = true;
            let _ = source.abort(&generation_id);
        }
    }
    registry.remove_wake(&generation_id);
    // A parked consumer must observe the disconnect.
    forwarder.finish();
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, TryRecvError};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Wake, Waker};

    use super::{Forwarder, ProxyStream, Stream, WakerSlot};

    /// A waker that probes the channel at the instant it is woken: the
    /// order contract is observable only from the consumer's side.
    struct Probe {
        receiver: Mutex<Receiver<fr_wire::StreamChunk>>,
        woken: AtomicBool,
        saw_disconnect: AtomicBool,
    }

    impl Wake for Probe {
        fn wake(self: Arc<Self>) {
            let probed = self
                .receiver
                .lock()
                .expect("probe receiver lock")
                .try_recv();
            self.saw_disconnect.store(
                matches!(probed, Err(TryRecvError::Disconnected)),
                Ordering::SeqCst,
            );
            self.woken.store(true, Ordering::SeqCst);
        }
    }

    /// The final wake finds the channel already closed: a consumer woken
    /// by it re-polls against a dead sender instead of parking again with
    /// nobody left to wake it (the lost-wakeup that hung `fr headless`).
    #[test]
    fn finish_closes_the_channel_before_waking() {
        let (sender, receiver) = mpsc::channel();
        let probe = Arc::new(Probe {
            receiver: Mutex::new(receiver),
            woken: AtomicBool::new(false),
            saw_disconnect: AtomicBool::new(false),
        });
        let slot: WakerSlot = Arc::new(Mutex::new(Some(Waker::from(Arc::clone(&probe)))));
        Forwarder {
            sender,
            waker: slot,
        }
        .finish();
        assert!(probe.woken.load(Ordering::SeqCst), "the parked waker fires");
        assert!(
            probe.saw_disconnect.load(Ordering::SeqCst),
            "the channel is already closed when the wake lands"
        );
    }

    /// A stream parked on an empty channel is woken by the final wake and
    /// then reports its end.
    #[test]
    fn finish_ends_a_parked_stream() {
        struct Flag(AtomicBool);
        impl Wake for Flag {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let (sender, receiver) = mpsc::channel();
        let slot: WakerSlot = Arc::new(Mutex::new(None));
        let forwarder = Forwarder {
            sender,
            waker: Arc::clone(&slot),
        };
        let mut stream = ProxyStream {
            receiver,
            waker: slot,
        };
        let flag = Arc::new(Flag(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&flag));
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Pending
        ));
        forwarder.finish();
        assert!(
            flag.0.load(Ordering::SeqCst),
            "the parked consumer is woken"
        );
        assert!(matches!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Ready(None)
        ));
    }
}
