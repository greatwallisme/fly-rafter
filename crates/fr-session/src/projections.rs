//! The `'session-projections'` service face over the dougong dynamic
//! channel (Q105′A seam; Q110′A′ folding ownership).
//!
//! M1 faces: register (1000) and state-of (1001). The two selectors sit in
//! the 1000+ band because the dougong dynamic channel hands one artifact
//! only the method selector — this service shares fr-session's export
//! table with `session-persistence`, so the bands stay disjoint.
//!
//! Folding ownership (Q110′A′): the registrant self-holds the fold. Its
//! own reads pull the log tail over the `session-persistence` read face
//! and fold locally; third-party reads relay here — the crate cold-reads
//! the stored log, hands the full log to the registrant's pure folding
//! endpoint (no outbound requires inside it), and returns the state. The
//! registry keeps no cell in M1 (a cached cell is a pure optimization to
//! add later; the incremental-advance refusal dsh carries with it — the
//! missing-seq text — is unreachable on the full-log relay and stays
//! documented, not dead-coded).
//!
//! The relay's outbound require rides the runtime grant the registrant
//! issued at its setup (D31); this artifact's inject list stays empty
//! because a static inject cannot name future registrants.
//!
//! Register refusals reuse the dsh session-projection texts verbatim
//! (the `JSON.stringify` quoted-key forms); refs counting mirrors the
//! dsh sharing rule — a matching-version re-register increments the
//! count, and the dispose leg rides the registrant activation's teardown
//! (the Phase 18 assembler's wiring), not a wire method.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::vocabulary::EventVocabulary;

/// The provided-service name consumers inject (the dsh domain service
/// name; cross-plugin constants live in the fr-wire table and the tests
/// below pin equality with it).
pub const SERVICE_NAME: &str = "session-projections";

/// Method selector: register one projection unit.
pub const METHOD_REGISTER: u32 = 1000;
/// Method selector: read one unit's state through the relay.
pub const METHOD_STATE_OF: u32 = 1001;

/// One registration's pure folding endpoint on the registrant's service
/// (full log to state; no outbound requires inside).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FoldEndpoint {
    /// The registrant's provided-service name.
    pub service: String,
    /// The folding method selector.
    pub method: u32,
}

/// The cold-read log one relay pass hands the fold endpoint.
///
/// The session header, the inherited prefix length, and the full typed
/// event list — the wire form of dsh's `init(header,
/// inheritedEventCount)` plus the `apply` event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdLog {
    /// The logical session header.
    pub header: Value,
    /// The exact inherited prefix length.
    pub inherited_event_count: u64,
    /// The preserved events in log order.
    pub events: Vec<Value>,
}

/// The read-source seam: cold-reads one session's stored log against a
/// vocabulary snapshot taken per call (event types declared after setup
/// must still decode).
pub trait ColdLogRead: Send + Sync {
    /// Reads the stored log, or `None` when no log exists for the id.
    ///
    /// # Errors
    ///
    /// Returns the persistence-layer refusal text.
    fn cold_read(&self, id: &str, known: &EventVocabulary) -> Result<Option<ColdLog>, String>;
}

/// The fold-call seam: invokes one registrant's pure folding endpoint.
pub trait FoldCaller: Send + Sync {
    /// Calls the endpoint with the full log and returns its state value.
    ///
    /// # Errors
    ///
    /// Returns the transport or decode refusal text.
    fn call_fold(&self, endpoint: &FoldEndpoint, log: &ColdLog) -> Result<Value, String>;
}

/// One registered unit: the fold endpoint and the sharing count.
#[derive(Debug)]
struct Registration {
    state_version: u64,
    fold: FoldEndpoint,
    refs: u64,
}

/// The `'session-projections'` service over one registry and the two
/// seams.
pub struct Projections {
    registrations: HashMap<String, Registration>,
    read: Arc<dyn ColdLogRead>,
    fold: Arc<dyn FoldCaller>,
}

impl std::fmt::Debug for Projections {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Projections")
            .field("registrations", &self.registrations)
            .finish_non_exhaustive()
    }
}

impl Projections {
    /// Assembles the service over its read source and fold caller.
    #[must_use]
    pub fn new(read: Arc<dyn ColdLogRead>, fold: Arc<dyn FoldCaller>) -> Self {
        Self {
            registrations: HashMap::new(),
            read,
            fold,
        }
    }

    /// Serves one method invocation. The vocabulary snapshot comes from
    /// the caller per call, so event types declared after this service
    /// was assembled still decode on the relay's cold read.
    ///
    /// # Errors
    ///
    /// Returns the failure text the host surfaces as `STATUS_FAILED`.
    pub fn serve(
        &mut self,
        method: u32,
        payload: &[u8],
        known: &EventVocabulary,
    ) -> Result<Vec<u8>, String> {
        match method {
            METHOD_REGISTER => self.serve_register(payload),
            METHOD_STATE_OF => self.serve_state_of(payload, known),
            other => Err(format!("unknown session-projections method {other}")),
        }
    }

    /// The register face: validates the unit, then inserts or counts it.
    fn serve_register(&mut self, payload: &[u8]) -> Result<Vec<u8>, String> {
        let request: Value = serde_json::from_slice(payload)
            .map_err(|error| format!("register payload must be JSON: {error}"))?;
        let key = word(&request, "key")?;
        let raw_version = request.get("stateVersion").cloned().unwrap_or(Value::Null);
        let state_version = parse_state_version(&key, &raw_version)?;
        let fold: FoldEndpoint = serde_json::from_value(
            request
                .get("fold")
                .cloned()
                .ok_or_else(|| "register requires a fold endpoint".to_owned())?,
        )
        .map_err(|error| format!("register fold endpoint decode: {error}"))?;
        match self.registrations.entry(key.clone()) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                if existing.state_version != state_version {
                    return Err(format!(
                        "session projection key {} is already registered at stateVersion {}; refusing to share it with stateVersion {}",
                        quote(&key),
                        existing.state_version,
                        state_version
                    ));
                }
                existing.refs += 1;
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Registration {
                    state_version,
                    fold,
                    refs: 1,
                });
            }
        }
        Ok(serde_json::to_vec(&Value::Null).unwrap_or_default())
    }

    /// The state-of face: the third-party relay (Q110′A′).
    fn serve_state_of(&self, payload: &[u8], known: &EventVocabulary) -> Result<Vec<u8>, String> {
        let request: Value = serde_json::from_slice(payload)
            .map_err(|error| format!("state-of payload must be JSON: {error}"))?;
        let id = word(&request, "id")?;
        let key = word(&request, "key")?;
        let Some(registration) = self.registrations.get(&key) else {
            // dsh `stateOf` returns undefined for an unregistered key; the
            // wire form omits the state key (the approval override-of
            // absence judgment).
            return Ok(serde_json::to_vec(&serde_json::json!({})).unwrap_or_default());
        };
        let Some(log) = self.read.cold_read(&id, known)? else {
            return Err(format!("session \"{id}\" not found"));
        };
        let state = self.fold.call_fold(&registration.fold, &log)?;
        Ok(serde_json::to_vec(&serde_json::json!({ "state": state })).unwrap_or_default())
    }
}

/// Decodes one required string field.
fn word(request: &Value, key: &str) -> Result<String, String> {
    request.get(key).and_then(Value::as_str).map_or_else(
        || Err(format!("{key} must be a string")),
        |word| Ok(word.to_owned()),
    )
}

/// Renders one key the way dsh's `JSON.stringify` does (quoted).
fn quote(key: &str) -> String {
    serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\""))
}

/// `Number.MAX_SAFE_INTEGER`: the dsh `Number.isSafeInteger` bound (the
/// integer form for u64-domain compares, the f64 form for the float
/// domain — both literals, no lossy casts).
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// The f64 view of [`MAX_SAFE_INTEGER`].
const MAX_SAFE_INTEGER_F64: f64 = 9_007_199_254_740_991.0;

/// Parses one state version with the dsh `Number.isSafeInteger`
/// semantics: a non-negative safe integer (the JSON form `2.0` is the JS
/// number `2` and passes; `-0` is the JS number `0`).
fn parse_state_version(key: &str, raw: &Value) -> Result<u64, String> {
    let accepted = raw
        .as_u64()
        .filter(|version| *version <= MAX_SAFE_INTEGER)
        .or_else(|| {
            let number = raw.as_f64()?;
            if number == 0.0 {
                return Some(0);
            }
            if number.fract() != 0.0 || number < 0.0 || number > MAX_SAFE_INTEGER_F64 {
                return None;
            }
            // Inside the safe bound the Display rendering is exact
            // digits, so the float form ("2.0") round-trips through it.
            number.to_string().parse::<u64>().ok()
        });
    accepted.map_or_else(
        || {
            Err(format!(
                "session projection {} stateVersion must be a non-negative integer, got {}",
                quote(key),
                render_js_value(raw)
            ))
        },
        Ok,
    )
}

/// Renders a JSON value the way a JS template literal would `String()` it
/// (the error-text argument form: bare words, unquoted strings).
fn render_js_value(raw: &Value) -> String {
    match raw {
        // Rust's f64 Display prints the same shortest round-trip digits
        // JS String() does — integral floats without a fraction mark;
        // exponent notation beyond 1e21 is a documented corner Rust
        // prints in full decimal form (the json_schema judgment).
        Value::Number(number) => number
            .as_f64()
            .map_or_else(|| number.to_string(), |number| format!("{number}")),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ColdLog, ColdLogRead, FoldCaller, FoldEndpoint, METHOD_REGISTER, METHOD_STATE_OF,
        Projections,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;

    /// A scripted read source: one fixed log per id, `None` otherwise.
    struct FixedRead {
        log: Option<ColdLog>,
    }

    impl ColdLogRead for FixedRead {
        fn cold_read(
            &self,
            _id: &str,
            _known: &crate::vocabulary::EventVocabulary,
        ) -> Result<Option<ColdLog>, String> {
            Ok(self.log.clone())
        }
    }

    /// A scripted fold caller recording the endpoint it served.
    struct RecordingFold {
        state: Value,
        seen: std::sync::Mutex<Vec<(FoldEndpoint, ColdLog)>>,
    }

    impl FoldCaller for RecordingFold {
        fn call_fold(&self, endpoint: &FoldEndpoint, log: &ColdLog) -> Result<Value, String> {
            self.seen
                .lock()
                .expect("lock")
                .push((endpoint.clone(), log.clone()));
            Ok(self.state.clone())
        }
    }

    fn projections(log: Option<ColdLog>, state: Value) -> (Projections, Arc<RecordingFold>) {
        let fold = Arc::new(RecordingFold {
            state,
            seen: std::sync::Mutex::new(Vec::new()),
        });
        (
            Projections::new(
                Arc::new(FixedRead { log }),
                Arc::clone(&fold) as Arc<dyn FoldCaller>,
            ),
            fold,
        )
    }

    fn register(
        service: &mut Projections,
        key: &str,
        version: &Value,
        method: u32,
    ) -> Result<Vec<u8>, String> {
        let payload = json!({
            "key": key,
            "stateVersion": version,
            "fold": { "service": "permission-presets", "method": method },
        });
        service.serve(
            METHOD_REGISTER,
            payload.to_string().as_bytes(),
            &crate::vocabulary::EventVocabulary::core(),
        )
    }

    #[test]
    fn wire_constants_match_the_fr_wire_table() {
        assert_eq!(
            super::SERVICE_NAME,
            fr_wire::services::session_projections::SERVICE_NAME
        );
        assert_eq!(
            super::METHOD_REGISTER,
            fr_wire::services::session_projections::METHOD_REGISTER
        );
        assert_eq!(
            super::METHOD_STATE_OF,
            fr_wire::services::session_projections::METHOD_STATE_OF
        );
    }

    #[test]
    fn register_accepts_a_unit_and_rejects_bad_versions() {
        let (mut service, _fold) = projections(None, json!("unused"));
        register(&mut service, "permissions", &json!(2), 2).expect("first register");
        // The same version re-registers (the dsh refs-sharing rule).
        register(&mut service, "permissions", &json!(2), 2).expect("re-register shares");
        // The JSON form 2.0 is the JS number 2.
        register(&mut service, "other", &json!(2.0), 4).expect("integer-valued float passes");

        let error = register(&mut service, "bad", &json!(-1), 2).expect_err("negative");
        assert_eq!(
            error,
            "session projection \"bad\" stateVersion must be a non-negative integer, got -1"
        );
        let error = register(&mut service, "bad", &json!(2.5), 2).expect_err("fraction");
        assert_eq!(
            error,
            "session projection \"bad\" stateVersion must be a non-negative integer, got 2.5"
        );
        let error = register(&mut service, "bad", &json!("2"), 2).expect_err("string");
        assert_eq!(
            error,
            "session projection \"bad\" stateVersion must be a non-negative integer, got 2"
        );
        let error = register(&mut service, "bad", &json!(u64::MAX), 2).expect_err("unsafe range");
        assert!(
            error.ends_with("must be a non-negative integer, got 18446744073709552000"),
            "over-safe-integer renders its full f64 digits: {error}"
        );
    }

    #[test]
    fn register_refuses_a_version_conflict_verbatim() {
        let (mut service, _fold) = projections(None, json!("unused"));
        register(&mut service, "permissions", &json!(2), 2).expect("first register");
        let error = register(&mut service, "permissions", &json!(3), 2).expect_err("conflict");
        assert_eq!(
            error,
            "session projection key \"permissions\" is already registered at stateVersion 2; refusing to share it with stateVersion 3"
        );
    }

    #[test]
    fn state_of_relays_the_full_log_to_the_fold_endpoint() {
        let log = ColdLog {
            header: json!({ "id": "s1" }),
            inherited_event_count: 0,
            events: vec![json!({ "type": "permission/preset", "data": { "preset": "read-only" } })],
        };
        let (mut service, fold) = projections(Some(log), json!({ "preset": "read-only" }));
        register(&mut service, "permissions", &json!(2), 2).expect("register");

        let reply = service
            .serve(
                METHOD_STATE_OF,
                json!({ "id": "s1", "key": "permissions" })
                    .to_string()
                    .as_bytes(),
                &crate::vocabulary::EventVocabulary::core(),
            )
            .expect("relay");
        assert_eq!(
            serde_json::from_slice::<Value>(&reply).expect("json"),
            json!({ "state": { "preset": "read-only" } })
        );
        let seen = fold.seen.lock().expect("lock");
        assert_eq!(seen.len(), 1, "exactly one fold call");
        assert_eq!(seen[0].0.service, "permission-presets");
        assert_eq!(seen[0].0.method, 2);
        assert_eq!(seen[0].1.events.len(), 1);
    }

    #[test]
    fn state_of_answers_absence_and_unknown_sessions() {
        let (mut service, _fold) = projections(None, json!("unused"));
        register(&mut service, "permissions", &json!(2), 2).expect("register");

        // An unregistered key folds to the omitted-state reply.
        let reply = service
            .serve(
                METHOD_STATE_OF,
                json!({ "id": "s1", "key": "nope" }).to_string().as_bytes(),
                &crate::vocabulary::EventVocabulary::core(),
            )
            .expect("absence");
        assert_eq!(
            serde_json::from_slice::<Value>(&reply).expect("json"),
            json!({})
        );

        // A registered key with no stored log fails closed.
        let error = service
            .serve(
                METHOD_STATE_OF,
                json!({ "id": "s1", "key": "permissions" })
                    .to_string()
                    .as_bytes(),
                &crate::vocabulary::EventVocabulary::core(),
            )
            .expect_err("unknown session");
        assert_eq!(error, "session \"s1\" not found");
    }

    #[test]
    fn malformed_payloads_and_unknown_methods_fail_with_text() {
        let (mut service, _fold) = projections(None, json!("unused"));
        let known = crate::vocabulary::EventVocabulary::core();
        assert!(
            service
                .serve(METHOD_REGISTER, b"not json", &known)
                .expect_err("bad json")
                .contains("register payload must be JSON")
        );
        assert!(
            service
                .serve(METHOD_STATE_OF, b"not json", &known)
                .expect_err("bad json")
                .contains("state-of payload must be JSON")
        );
        assert_eq!(
            service
                .serve(1002, b"{}", &known)
                .expect_err("unknown method"),
            "unknown session-projections method 1002"
        );
    }
}

// Rust guideline compliant 2026-10-29
