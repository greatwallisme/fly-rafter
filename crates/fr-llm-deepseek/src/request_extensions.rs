//! Plugin-contributed request-field preparation and post-acceptance
//! commit (dsh `request-extensions.ts`; Q72-A).
//!
//! The dsh mechanism merges
//! contributions from the `deepseekLlmApiExtensions` service; fr's M1 has
//! no service provider, so the production path is the no-op default and
//! the full mechanism is kept for structural 1:1 parity.

use crate::codes::REQUEST_EXTENSION_CODE;
use fr_llm::adapter::{AbortSignal, CallPurpose};
use fr_llm::error::LlmError;
use serde_json::Value;
use std::sync::Arc;

/// Reports fields omitted from the payload after a merge-serialization
/// failure (dsh `onOmitted`).
pub type OmittedReporter = Arc<dyn Fn(&[String], &str) + Send + Sync>;

/// Reports an acceptance failure after HTTP 2xx (dsh `onUnaccepted`).
pub type UnacceptedReporter = Arc<dyn Fn(&str) + Send + Sync>;

/// The contributed preparation seam as the bare reference the preparer
/// consumes (dsh `prepareExtensions`).
pub type PreparedExtensionSeam<'a> =
    dyn Fn(&ExtensionRequest) -> Result<PreparedExtensions, String> + 'a;

/// Exact serialized request facts visible to extension providers (dsh
/// `DeepSeekLlmApiExtensionRequest`).
#[derive(Debug)]
pub struct ExtensionRequest<'a> {
    /// Base Messages request body before extension fields are merged.
    pub body: &'a Value,
    /// Session identity carried by the model request, when present.
    pub session_id: Option<&'a str>,
    /// Auxiliary request classification, when present.
    pub purpose: Option<CallPurpose>,
    /// Cancellation for request preparation; providers must stop promptly
    /// after abort.
    pub signal: Option<&'a AbortSignal>,
}

/// All fields prepared for one request plus their joint acceptance
/// transaction (dsh `PreparedDeepSeekLlmApiExtensions`).
pub struct PreparedExtensions {
    // Manual `Debug` below: the boxed commit is not `Debug`.
    /// Detached top-level fields to merge into the base request.
    pub fields: serde_json::Map<String, Value>,
    /// Commit captured state after HTTP 2xx; an error render is reported,
    /// never fatal.
    pub accept: Box<dyn FnOnce() -> Result<(), String> + Send>,
}

impl Default for PreparedExtensions {
    /// The no-op default: no fields, nothing to commit (the production
    /// M1 path, where no extension service is mounted).
    fn default() -> Self {
        Self {
            fields: serde_json::Map::new(),
            accept: Box::new(|| Ok(())),
        }
    }
}

/// The HTTP payload beside its post-acceptance commit (dsh's
/// `{payload, accept()}` pair); [`Self::accept`] reports its own failures
/// and never fails the request.
pub struct ExtensionsOutcome {
    /// The serialized request body to send.
    pub payload: String,
    accept: Box<dyn FnOnce() + Send>,
}

impl std::fmt::Debug for PreparedExtensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedExtensions")
            .field("fields", &self.fields)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ExtensionsOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionsOutcome")
            .field("payload", &self.payload)
            .finish_non_exhaustive()
    }
}

impl ExtensionsOutcome {
    /// Invoke only after a successful HTTP response (dsh `accept()`).
    pub fn accept(self) {
        (self.accept)();
    }
}

/// Merge contributions without replacing Messages fields (dsh
/// `prepareRequestExtensions`).
///
/// A rejected preparation or a base-field
/// collision reports `REQUEST_EXTENSION` with the cause appended to the
/// message.
///
/// When the merged request fails to serialize, the payload is
/// the base request alone and acceptance is a no-op, so contributors
/// resend their unaccepted state on a later request. An acceptance
/// failure is reported and does not fail the request.
///
/// # Errors
/// [`REQUEST_EXTENSION_CODE`] for a rejected preparation or a base-field
/// collision.
///
/// # Panics
/// Only if the static error fields fail [`LlmError::try_new`] validation
/// and string serialization fails (both programmer errors that cannot
/// occur for these constant texts and field names).
pub fn prepare_request_extensions(
    body: &Value,
    options: &ExtensionRequest<'_>,
    prepare: Option<&PreparedExtensionSeam<'_>>,
    on_omitted: &OmittedReporter,
    on_unaccepted: UnacceptedReporter,
) -> Result<ExtensionsOutcome, LlmError> {
    let mut extensions = PreparedExtensions::default();
    if let Some(prepare) = prepare {
        match prepare(options) {
            Ok(prepared) => extensions = prepared,
            Err(chain) => {
                return Err(LlmError::try_new(
                    &format!("DeepSeek request extension preparation failed: {chain}"),
                    REQUEST_EXTENSION_CODE,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("extension fields are static"));
            }
        }
    }
    let field_names: Vec<String> = extensions.fields.keys().cloned().collect();
    for field in &field_names {
        let collides = body
            .as_object()
            .is_some_and(|base| base.contains_key(field));
        if collides {
            let rendered = serde_json::to_string(field).expect("string serialization cannot fail");
            return Err(LlmError::try_new(
                &format!(
                    "DeepSeek request extension field {rendered} collides with the base request"
                ),
                REQUEST_EXTENSION_CODE,
                None,
                None,
                None,
                None,
            )
            .expect("collision fields are static"));
        }
    }
    let mut merged = body.as_object().cloned().unwrap_or_default();
    for (name, value) in &extensions.fields {
        merged.insert(name.clone(), value.clone());
    }
    let payload = match serde_json::to_string(&merged) {
        Ok(payload) => payload,
        Err(error) => {
            // A base request that cannot serialize throws here, before
            // any field is reported as omitted (dsh ordering). The no-op
            // acceptance makes contributors resend on a later request.
            let base = serde_json::to_string(body).unwrap_or_else(|_| "null".to_owned());
            on_omitted(&field_names, &error.to_string());
            return Ok(ExtensionsOutcome {
                payload: base,
                accept: Box::new(|| {}),
            });
        }
    };
    let accept = extensions.accept;
    Ok(ExtensionsOutcome {
        payload,
        accept: Box::new(move || {
            if let Err(error) = accept() {
                on_unaccepted(&error);
            }
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body() -> Value {
        json!({"model": "deepseek-flash", "max_tokens": 64})
    }

    fn silent() -> (OmittedReporter, UnacceptedReporter) {
        (
            Arc::new(|_fields: &[String], _error: &str| {}),
            Arc::new(|_error: &str| {}),
        )
    }

    /// The production no-op: no seam, base payload verbatim, acceptance a
    /// no-op.
    #[test]
    fn no_seam_yields_base_payload_and_noop_accept() {
        let (on_omitted, on_unaccepted) = silent();
        let outcome = prepare_request_extensions(
            &body(),
            &ExtensionRequest {
                body: &body(),
                session_id: None,
                purpose: None,
                signal: None,
            },
            None,
            &on_omitted,
            on_unaccepted,
        )
        .unwrap();
        assert_eq!(outcome.payload, body().to_string());
        outcome.accept();
    }

    /// A rejected preparation reports `REQUEST_EXTENSION` with the cause
    /// chain appended.
    #[test]
    fn preparation_failure_pins_message_and_code() {
        let (on_omitted, on_unaccepted) = silent();
        let prepare = |_request: &ExtensionRequest| -> Result<PreparedExtensions, String> {
            Err("outer: inner cause".to_owned())
        };
        let Err(error) = prepare_request_extensions(
            &body(),
            &ExtensionRequest {
                body: &body(),
                session_id: None,
                purpose: None,
                signal: None,
            },
            Some(&prepare),
            &on_omitted,
            on_unaccepted,
        ) else {
            panic!("rejected preparation must fail");
        };
        assert_eq!(error.code(), REQUEST_EXTENSION_CODE);
        assert_eq!(
            error.message(),
            "DeepSeek request extension preparation failed: outer: inner cause"
        );
    }

    /// A field colliding with the base request reports `REQUEST_EXTENSION`
    /// with the JSON-rendered field name.
    #[test]
    fn collision_pins_message_and_code() {
        let (on_omitted, on_unaccepted) = silent();
        let prepare = |_request: &ExtensionRequest| -> Result<PreparedExtensions, String> {
            let mut fields = serde_json::Map::new();
            fields.insert("max_tokens".to_owned(), json!(1));
            Ok(PreparedExtensions {
                fields,
                accept: Box::new(|| Ok(())),
            })
        };
        let Err(error) = prepare_request_extensions(
            &body(),
            &ExtensionRequest {
                body: &body(),
                session_id: None,
                purpose: None,
                signal: None,
            },
            Some(&prepare),
            &on_omitted,
            on_unaccepted,
        ) else {
            panic!("base-field collision must fail");
        };
        assert_eq!(error.code(), REQUEST_EXTENSION_CODE);
        assert_eq!(
            error.message(),
            "DeepSeek request extension field \"max_tokens\" collides with the base request"
        );
    }

    /// Successful preparation merges fields into the payload and the
    /// commit runs exactly once after acceptance; a commit failure is
    /// reported through the callback instead of failing.
    #[test]
    fn success_merges_and_commit_reports_failures() {
        let accepted = std::sync::atomic::AtomicUsize::new(0);
        let accepted_cell = std::sync::Arc::new(accepted);
        let counter = std::sync::Arc::clone(&accepted_cell);
        let (on_omitted, _) = silent();
        let unaccepted: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let unaccepted_cell = std::sync::Arc::new(unaccepted);
        let reporter = std::sync::Arc::clone(&unaccepted_cell);
        let on_unaccepted: UnacceptedReporter = Arc::new(move |error: &str| {
            reporter.lock().expect("log").push(error.to_owned());
        });
        let prepare = |_request: &ExtensionRequest| -> Result<PreparedExtensions, String> {
            let mut fields = serde_json::Map::new();
            fields.insert("x_fr_extra".to_owned(), json!({"k": 1}));
            let counter = std::sync::Arc::clone(&counter);
            Ok(PreparedExtensions {
                fields,
                accept: Box::new(move || {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }),
            })
        };
        let outcome = prepare_request_extensions(
            &body(),
            &ExtensionRequest {
                body: &body(),
                session_id: Some("session-1"),
                purpose: Some(CallPurpose::Compaction),
                signal: None,
            },
            Some(&prepare),
            &on_omitted,
            on_unaccepted,
        )
        .unwrap();
        let merged: Value = serde_json::from_str(&outcome.payload).unwrap();
        assert_eq!(merged["model"], "deepseek-flash");
        assert_eq!(merged["x_fr_extra"]["k"], 1);
        assert_eq!(accepted_cell.load(std::sync::atomic::Ordering::SeqCst), 0);
        outcome.accept();
        assert_eq!(accepted_cell.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(unaccepted_cell.lock().expect("log").is_empty());

        // A failing commit is reported once and never fails the request.
        let (on_omitted, _) = silent();
        let unaccepted_cell = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let reporter = std::sync::Arc::clone(&unaccepted_cell);
        let on_unaccepted: UnacceptedReporter = Arc::new(move |error: &str| {
            reporter.lock().expect("log").push(error.to_owned());
        });
        let prepare = |_request: &ExtensionRequest| -> Result<PreparedExtensions, String> {
            Ok(PreparedExtensions {
                fields: serde_json::Map::new(),
                accept: Box::new(|| Err("commit exploded".to_owned())),
            })
        };
        let outcome = prepare_request_extensions(
            &body(),
            &ExtensionRequest {
                body: &body(),
                session_id: None,
                purpose: None,
                signal: None,
            },
            Some(&prepare),
            &on_omitted,
            on_unaccepted,
        )
        .unwrap();
        outcome.accept();
        assert_eq!(
            *unaccepted_cell.lock().expect("log"),
            vec!["commit exploded".to_owned()]
        );
    }
}

// Rust guideline compliant 2026-10-26
