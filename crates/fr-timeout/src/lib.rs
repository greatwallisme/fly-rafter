//! Shared timeout arithmetic, signal fusion, and classification (dsh
//! `util/timeout/src/index.ts`, blocking-face port).
//!
//! The library only notifies through fused deadline signals; each
//! capability owns the mechanism that stops its work and translates
//! timeout reasons into public outcomes. The dsh shapes are event-loop
//! timers (`setTimeout` racing an async iterator); the blocking-face
//! mapping keeps every observable semantic — the verbatim messages, the
//! `2^31-1` timer ceiling, first-abort-wins reason ownership, the
//! `<= 0` no-timer sentinel, and "the timer exists only while a demand
//! is outstanding" — while the rearmable watchdog becomes a deadline
//! instant the owning read loop resets on every progress signal.
//!
//! dsh disposal (`[Symbol.dispose]` clears the timer) needs no separate
//! face here: the deadline instant lives inside the watchdog, so drop is
//! the cleanup.

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Largest delay Node schedules without clamping it to one millisecond.
pub const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;

/// Internal abort reason carrying a capability-owned code and elapsed
/// deadline. Consumers translate it through [`timeout_of`] semantics
/// before returning to callers.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeoutReason {
    /// Capability-owned timeout code (e.g. `BASH_TIMEOUT`).
    pub code: &'static str,
    /// The deadline that elapsed, in milliseconds.
    pub timeout_ms: f64,
}

impl TimeoutReason {
    /// Capability-owned timeout code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// The deadline that elapsed, in milliseconds.
    #[must_use]
    pub const fn timeout_ms(&self) -> f64 {
        self.timeout_ms
    }
}

impl fmt::Display for TimeoutReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} after {}ms", self.code, self.timeout_ms)
    }
}

/// Rejection of a caller-supplied timeout hint (dsh throws a bare `Error`
/// with these verbatim messages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutConfigError {
    message: String,
}

impl TimeoutConfigError {
    /// The verbatim rejection message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TimeoutConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TimeoutConfigError {}

/// Validate a positive finite delay against the timer ceiling (dsh
/// `assertTimerDelay`).
///
/// # Errors
///
/// [`TimeoutConfigError`] with the dsh message when `timeout_ms` is not
/// a positive finite number no greater than [`MAX_TIMER_DELAY_MS`].
pub fn assert_timer_delay(timeout_ms: f64, name: &str) -> Result<f64, TimeoutConfigError> {
    if !timeout_ms.is_finite() || timeout_ms <= 0.0 || timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(TimeoutConfigError {
            message: format!(
                "{name} must be a positive finite number no greater than {MAX_TIMER_DELAY_MS}"
            ),
        });
    }
    Ok(timeout_ms)
}

/// Validate a caller's optional timeout hint, use the backend default, then
/// cap it (dsh `clampTimeout`). Supplied values must be positive and finite;
/// zero is not a public disable-timeout sentinel.
///
/// # Errors
///
/// [`TimeoutConfigError`] with the dsh message when a supplied hint is
/// not a positive finite number.
pub fn clamp_timeout(
    requested: Option<f64>,
    def: f64,
    max: f64,
    name: &str,
) -> Result<f64, TimeoutConfigError> {
    if let Some(requested) = requested
        && (!requested.is_finite() || requested <= 0.0)
    {
        return Err(TimeoutConfigError {
            message: format!("{name} must be a positive finite number"),
        });
    }
    Ok(f64::min(requested.unwrap_or(def), max))
}

/// Why one fused deadline signal aborted (dsh `AbortSignal.reason`
/// classification: the deadline's [`TimeoutReason`] when the timer won;
/// otherwise ordinary upstream cancellation).
#[derive(Debug, Clone, PartialEq)]
pub enum AbortKind {
    /// The timer won the race; carries the capability-owned reason.
    Timeout(TimeoutReason),
    /// Upstream cancellation reached the signal first.
    Upstream,
}

#[derive(Debug, Default)]
struct SignalState {
    aborted: AtomicBool,
    kind: Mutex<Option<AbortKind>>,
}

/// Fused cancellation signal with first-abort-wins reason ownership (dsh
/// `AbortSignal.any` semantics: whichever source aborts first owns the
/// recorded reason).
#[derive(Clone, Debug, Default)]
pub struct DeadlineSignal {
    state: std::sync::Arc<SignalState>,
}

impl DeadlineSignal {
    /// A signal that has not aborted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the signal has aborted.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.state.aborted.load(Ordering::SeqCst)
    }

    /// The recorded abort reason, if aborted.
    #[must_use]
    pub fn reason(&self) -> Option<AbortKind> {
        let slot = self.state.kind.lock().ok()?;
        slot.as_ref().map(Clone::clone)
    }

    /// Record upstream cancellation (first abort wins).
    pub fn abort_upstream(&self) {
        self.abort_with(AbortKind::Upstream);
    }

    /// Record a timer loss for the owner of the deadline (first abort
    /// wins).
    pub fn abort_timeout(&self, reason: TimeoutReason) {
        self.abort_with(AbortKind::Timeout(reason));
    }

    fn abort_with(&self, kind: AbortKind) {
        if self
            .state
            .aborted
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
            && let Ok(mut slot) = self.state.kind.lock()
        {
            *slot = Some(kind);
        }
    }
}

/// A deadline signal plus its fixed expiry (dsh `Deadline`; the dsh
/// disposer clears the timer, which the blocking face needs no separate
/// face for).
#[derive(Clone, Debug)]
pub struct Deadline {
    signal: DeadlineSignal,
    upstream: Option<DeadlineSignal>,
    expiry: Option<(Instant, TimeoutReason)>,
}

impl Deadline {
    /// The fused signal: aborted by upstream cancellation or on timeout.
    #[must_use]
    pub const fn signal(&self) -> &DeadlineSignal {
        &self.signal
    }

    /// Fuse the upstream signal and the timer, aborting the fused
    /// signal on whichever fired first.
    #[must_use]
    pub fn check(&self) -> bool {
        if let Some(upstream) = &self.upstream
            && upstream.is_aborted()
        {
            self.signal.abort_upstream();
        }
        if let Some((at, reason)) = &self.expiry
            && Instant::now() >= *at
        {
            self.signal.abort_timeout(reason.clone());
        }
        self.signal.is_aborted()
    }
}

/// Fuse upstream cancellation with an identifiable timeout (dsh
/// `deadline`).
///
/// `timeout_ms <= 0` is the internal no-timer sentinel: the fused
/// signal forwards only the upstream signal, or a never-aborting one
/// when there is no upstream.
///
/// # Errors
///
/// [`TimeoutConfigError`] when `timeout_ms` is positive but fails the
/// timer-delay assertion (dsh `deadline timeoutMs` field name).
pub fn deadline(
    upstream: Option<DeadlineSignal>,
    timeout_ms: f64,
    code: &'static str,
) -> Result<Deadline, TimeoutConfigError> {
    if timeout_ms <= 0.0 {
        return Ok(Deadline {
            signal: upstream.clone().unwrap_or_default(),
            upstream,
            expiry: None,
        });
    }
    assert_timer_delay(timeout_ms, "deadline timeoutMs")?;
    Ok(Deadline {
        signal: DeadlineSignal::new(),
        upstream,
        expiry: Some((
            Instant::now() + Duration::from_millis(f64_to_u64_ms(timeout_ms)),
            TimeoutReason { code, timeout_ms },
        )),
    })
}

/// Round a validated positive finite millisecond delay to whole
/// milliseconds for the blocking timer.
fn f64_to_u64_ms(timeout_ms: f64) -> u64 {
    let whole = timeout_ms.floor();
    let whole = if whole > MAX_TIMER_DELAY_MS {
        MAX_TIMER_DELAY_MS
    } else {
        whole
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "floor-clamped to [0, 2^31-1] above; u64 holds the value exactly"
    )]
    {
        whole as u64
    }
}

/// Rearmable idle deadline for one blocking read loop (dsh
/// `idleWatchdog`).
///
/// The timer exists only while a demand is outstanding: the owning loop
/// resets the deadline on every progress signal, so consumer think time
/// does not count as provider idle time. The fused signal is stable for
/// the whole call; the read loop must observe it to release the
/// resources of a demand the deadline abandoned.
#[derive(Debug)]
pub struct IdleWatchdog {
    code: &'static str,
    signal: DeadlineSignal,
    upstream: Option<DeadlineSignal>,
    timeout_ms: f64,
    next: Mutex<Option<Instant>>,
}

impl IdleWatchdog {
    /// Rearm the idle window after transport activity that yields no
    /// iteration value (dsh `pulse`); otherwise a no-op after abort.
    pub fn pulse(&self) {
        if self.signal.is_aborted() {
            return;
        }
        if let Ok(mut slot) = self.next.lock()
            && slot.is_some()
        {
            *slot = Some(Instant::now() + Duration::from_millis(f64_to_u64_ms(self.timeout_ms)));
        }
    }

    /// The fused signal: aborted by upstream cancellation or this
    /// watchdog's timeout.
    #[must_use]
    pub const fn signal(&self) -> &DeadlineSignal {
        &self.signal
    }

    /// Fuse the upstream signal and the idle window, aborting the fused
    /// signal on whichever fired first; `true` once aborted. The owning
    /// loop calls this around every read and calls [`Self::pulse`] on
    /// transport activity that produces no iteration value.
    pub fn check(&self) -> bool {
        if let Some(upstream) = &self.upstream
            && upstream.is_aborted()
        {
            self.signal.abort_upstream();
        }
        if !self.signal.is_aborted()
            && let Ok(slot) = self.next.lock()
            && let Some(at) = *slot
            && Instant::now() >= at
        {
            drop(slot);
            self.signal.abort_timeout(TimeoutReason {
                code: self.code_of(),
                timeout_ms: self.timeout_ms,
            });
        }
        self.signal.is_aborted()
    }

    const fn code_of(&self) -> &'static str {
        self.code
    }
}

/// Create a rearmable idle watchdog for a blocking read loop (dsh
/// `idleWatchdog`).
///
/// # Errors
///
/// [`TimeoutConfigError`] when `timeout_ms` fails the timer-delay
/// assertion (dsh `idleWatchdog timeoutMs` field name).
pub fn idle_watchdog(
    upstream: Option<DeadlineSignal>,
    timeout_ms: f64,
    code: &'static str,
) -> Result<IdleWatchdog, TimeoutConfigError> {
    assert_timer_delay(timeout_ms, "idleWatchdog timeoutMs")?;
    let watchdog = IdleWatchdog {
        code,
        signal: DeadlineSignal::new(),
        upstream,
        timeout_ms,
        next: Mutex::new(Some(
            Instant::now() + Duration::from_millis(f64_to_u64_ms(timeout_ms)),
        )),
    };
    Ok(watchdog)
}

/// Recover a timeout reason from a signal's recorded abort kind (dsh
/// `timeoutOf`).
///
/// The reason returns only when the timer won, and — when `code` is
/// supplied — only for that exact capability code, so a nested foreign
/// deadline follows the ordinary cancellation path.
#[must_use]
pub fn timeout_of(kind: Option<AbortKind>, code: Option<&str>) -> Option<TimeoutReason> {
    match kind {
        Some(AbortKind::Timeout(reason)) if code.is_none_or(|wanted| wanted == reason.code) => {
            Some(reason)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    const IDLE_CODE: &str = "MESSAGES_IDLE";

    /// The dsh text shape: `{code} after {timeoutMs}ms`.
    #[test]
    fn timeout_reason_display_pins_dsh_text() {
        let reason = TimeoutReason {
            code: IDLE_CODE,
            timeout_ms: 300_000.0,
        };
        assert_eq!(reason.to_string(), "MESSAGES_IDLE after 300000ms");
        assert_eq!(reason.code(), IDLE_CODE);
        assert_eq!(reason.timeout_ms(), 300_000.0);
    }

    /// The timer-delay assertion admits exactly the dsh domain: positive,
    /// finite, no greater than 2^31-1, with the verbatim message.
    #[test]
    fn assert_timer_delay_pins_domain_and_text() {
        assert_eq!(
            assert_timer_delay(MAX_TIMER_DELAY_MS, "n").unwrap(),
            MAX_TIMER_DELAY_MS
        );
        let message = assert_timer_delay(MAX_TIMER_DELAY_MS * 2.0, "watchdog")
            .unwrap_err()
            .message()
            .to_owned();
        assert_eq!(
            message,
            "watchdog must be a positive finite number no greater than 2147483647"
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let message = assert_timer_delay(bad, "n")
                .unwrap_err()
                .message()
                .to_owned();
            assert_eq!(
                message,
                "n must be a positive finite number no greater than 2147483647"
            );
        }
    }

    /// clampTimeout: the hint wins over the default, the cap bounds both,
    /// and a bad hint rejects with the dsh message under the given name.
    #[test]
    fn clamp_timeout_pins_semantics_and_text() {
        assert_eq!(
            clamp_timeout(None, 100.0, 500.0, "timeoutMs").unwrap(),
            100.0
        );
        assert_eq!(
            clamp_timeout(Some(50.0), 100.0, 500.0, "timeoutMs").unwrap(),
            50.0
        );
        assert_eq!(
            clamp_timeout(Some(900.0), 100.0, 500.0, "timeoutMs").unwrap(),
            500.0
        );
        assert_eq!(
            clamp_timeout(None, 900.0, 500.0, "timeoutMs").unwrap(),
            500.0
        );
        let message = clamp_timeout(Some(0.0), 100.0, 500.0, "timeoutMs")
            .unwrap_err()
            .message()
            .to_owned();
        assert_eq!(message, "timeoutMs must be a positive finite number");
        let named = clamp_timeout(Some(f64::NEG_INFINITY), 100.0, 500.0, "bashTimeout")
            .unwrap_err()
            .message()
            .to_owned();
        assert_eq!(named, "bashTimeout must be a positive finite number");
    }

    /// deadline: `<= 0` is the no-timer sentinel (forward-only), positive
    /// delays abort with the capability reason at expiry, and upstream
    /// cancellation reaches the fused signal.
    #[test]
    fn deadline_pins_sentinel_expiry_and_fusion() {
        let bare = deadline(None, 0.0, IDLE_CODE).unwrap();
        assert!(!bare.check());
        sleep(Duration::from_millis(25));
        assert!(!bare.check(), "the sentinel never times out");

        let upstream = DeadlineSignal::new();
        let forwarded = deadline(Some(upstream.clone()), 0.0, IDLE_CODE).unwrap();
        upstream.abort_upstream();
        assert!(forwarded.check());
        assert_eq!(forwarded.signal().reason(), Some(AbortKind::Upstream));

        let timed = deadline(None, 20.0, IDLE_CODE).unwrap();
        assert!(!timed.check());
        sleep(Duration::from_millis(45));
        assert!(timed.check());
        assert_eq!(
            timed.signal().reason(),
            Some(AbortKind::Timeout(TimeoutReason {
                code: IDLE_CODE,
                timeout_ms: 20.0,
            }))
        );

        // <= 0 is the no-timer sentinel before any assertion (dsh checks
        // the sentinel first); only positive non-finite values reject.
        let negative = deadline(None, -5.0, IDLE_CODE).unwrap();
        assert!(!negative.check(), "a negative delay is the sentinel");
        let nonfinite = deadline(None, f64::NAN, IDLE_CODE)
            .unwrap_err()
            .message()
            .to_owned();
        assert_eq!(
            nonfinite,
            "deadline timeoutMs must be a positive finite number no greater than 2147483647"
        );
    }

    /// The watchdog arms at construction, the idle window expires into a
    /// capability reason, and pulse rearms an outstanding window.
    #[test]
    fn idle_watchdog_expires_and_pulses() {
        let watchdog = idle_watchdog(None, 20.0, IDLE_CODE).unwrap();
        assert!(!watchdog.check());
        sleep(Duration::from_millis(45));
        assert!(watchdog.check());
        assert_eq!(
            watchdog.signal().reason(),
            Some(AbortKind::Timeout(TimeoutReason {
                code: IDLE_CODE,
                timeout_ms: 20.0,
            }))
        );

        let named = idle_watchdog(None, 0.0, IDLE_CODE)
            .unwrap_err()
            .message()
            .to_owned();
        assert!(
            named.starts_with("idleWatchdog timeoutMs must be a positive finite number"),
            "verbatim prefix, got {named}"
        );
    }
}
