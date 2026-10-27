//! Deterministic Messages image preparation for Files references and
//! bounded inline fallback (dsh `images.ts`).
//!
//! Retained image references
//! normalize before converting Messages content, every retained
//! occurrence must fit the route budget, and resolution records every
//! occurrence for failure diagnostics.

use crate::codes::UNSUPPORTED_CONTENT_CODE;
use crate::file_id::DeepSeekFileId;
use crate::request_files::{FileResolutionFailure, ImageWireLocation, RequestFiles};
use crate::request_pricing::resolve_request_image_target;
use fr_attachment::AttachmentError;
use fr_attachment::request_image::RequestImageAttachment;
use fr_llm::adapter::{AbortSignal, RequestMessage};
use fr_llm::error::{IMAGE_OFFLOAD_REQUIRED_CODE, LlmError};
use fr_llm::projection::{
    ImageRepresentation, ImageRequestBudget, content_has_image, offloaded_image_text,
    project_offloaded_images, required_image_offload,
};
use fr_wire::{AttachmentId, ContentBlock, ImageAttachmentRef, KnownContentBlock};
use std::collections::HashMap;

use fr_attachment::AttachmentRead;

/// Any image preparation failure (dsh throws either a raw `LlmError` or
/// the attachment store's `AttachmentError`).
#[derive(Debug)]
pub enum PrepareImagesError {
    /// A provider-neutral route failure.
    Llm(LlmError),
    /// An attachment-pipeline failure.
    Attachment(AttachmentError),
}

impl From<LlmError> for PrepareImagesError {
    fn from(error: LlmError) -> Self {
        Self::Llm(error)
    }
}

impl From<AttachmentError> for PrepareImagesError {
    fn from(error: AttachmentError) -> Self {
        Self::Attachment(error)
    }
}

impl PrepareImagesError {
    /// The user-readable failure text.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Llm(error) => error.message(),
            Self::Attachment(error) => error.message(),
        }
    }

    /// The stable machine-routing code.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Llm(error) => error.code(),
            Self::Attachment(error) => error.code(),
        }
    }
}

/// Projected history beside the prepared request images keyed by durable
/// attachment identity (dsh's `{messages, versions}` pair).
#[derive(Debug)]
pub struct PreparedImages {
    /// History with offloaded occurrences already projected to text.
    pub messages: Vec<RequestMessage>,
    /// Prepared request versions for every retained reference.
    pub versions: HashMap<AttachmentId, RequestImageAttachment>,
}

/// The request-local image budgets of one representation (dsh
/// `bounds`).
#[must_use]
pub const fn image_request_budget(
    connection: &crate::types::ConnectionOptions,
    representation: ImageRepresentation,
) -> ImageRequestBudget {
    match representation {
        ImageRepresentation::Raw => ImageRequestBudget {
            representation,
            max_bytes: Some(connection.max_request_files_bytes),
            max_images: Some(connection.max_images_per_request),
            byte_quantum: Some(connection.image_offload_byte_quantum),
            count_quantum: Some(connection.image_offload_count_quantum),
        },
        ImageRepresentation::Base64 => ImageRequestBudget {
            representation,
            max_bytes: Some(connection.max_inline_request_image_bytes),
            max_images: Some(connection.max_images_per_request),
            byte_quantum: Some(connection.inline_image_offload_byte_quantum),
            count_quantum: Some(connection.image_offload_count_quantum),
        },
    }
}

/// Which representation a fit check applies to.
const fn representation_name(representation: ImageRepresentation) -> &'static str {
    match representation {
        ImageRepresentation::Raw => "raw",
        ImageRepresentation::Base64 => "base64",
    }
}

/// Count additional oldest occurrences requiring durable offload at their
/// exact represented bytes (dsh `assertImagesFit`).
///
/// # Errors
/// [`IMAGE_OFFLOAD_REQUIRED_CODE`] with the shortfall count when the
/// projection exceeds the route budget.
///
/// # Panics
/// Only if the static offload error fields fail [`LlmError::try_new`]
/// validation (a programmer error that cannot occur for these constant
/// texts).
pub fn assert_images_fit<S: ::std::hash::BuildHasher>(
    messages: &[RequestMessage],
    versions: &HashMap<AttachmentId, RequestImageAttachment, S>,
    connection: &crate::types::ConnectionOptions,
    representation: ImageRepresentation,
) -> Result<(), LlmError> {
    let budget = image_request_budget(connection, representation);
    let offload_images = required_image_offload(messages, &budget, |reference| {
        versions
            .get(&reference.attachment_id)
            .map_or(reference.bytes, |version| version.bytes)
    });
    if offload_images > 0 {
        return Err(LlmError::try_new(
            &format!(
                "DeepSeek Messages {} request images exceed the route budget; {offload_images} more oldest occurrence(s) must be offloaded.",
                representation_name(representation)
            ),
            IMAGE_OFFLOAD_REQUIRED_CODE,
            None,
            None,
            None,
            Some(offload_images),
        )
        .expect("offload fields are static"));
    }
    Ok(())
}

/// Normalize retained image references before converting Messages content
/// (dsh `prepareImages`).
///
/// # Errors
/// [`PrepareImagesError`] for a non-vision route without attachment
/// service, images outside user/tool messages, a failed fit, a failed
/// normalization, or caller cancellation.
///
/// # Panics
/// Only if the static validation error fields fail [`LlmError::try_new`]
/// validation (a programmer error that cannot occur for these constant
/// texts).
pub fn prepare_images(
    history: &[RequestMessage],
    connection: &crate::types::ConnectionOptions,
    model_id: &str,
    attachments: Option<&dyn AttachmentRead>,
    access: &dyn Fn(&ImageAttachmentRef) -> Option<fr_llm::projection::ImageAttachmentAccess>,
    signal: Option<&AbortSignal>,
) -> Result<PreparedImages, PrepareImagesError> {
    let substitute = |reference: &ImageAttachmentRef| {
        offloaded_image_text(reference, access(reference).as_ref())
    };
    let messages = project_offloaded_images(history, &substitute);
    if !messages
        .iter()
        .any(|message| content_has_image(&message.content_blocks()))
    {
        return Ok(PreparedImages {
            messages,
            versions: HashMap::new(),
        });
    }
    // dsh folds "model missing", "not vision", and "no attachment store"
    // into one failure; fr mirrors that exactly.
    let Some(model) = connection.model(model_id).filter(|entry| {
        entry
            .input_modalities
            .as_ref()
            .is_some_and(|modalities| modalities.contains(&fr_llm::adapter::ModelModality::Image))
    }) else {
        return Err(LlmError::try_new(
            "DeepSeek Messages image input requires a vision model and attachment service",
            UNSUPPORTED_CONTENT_CODE,
            None,
            None,
            None,
            None,
        )
        .expect("vision fields are static")
        .into());
    };
    let Some(attachments) = attachments else {
        return Err(LlmError::try_new(
            "DeepSeek Messages image input requires a vision model and attachment service",
            UNSUPPORTED_CONTENT_CODE,
            None,
            None,
            None,
            None,
        )
        .expect("attachment fields are static")
        .into());
    };
    if messages.iter().any(|message| {
        let role = message.role_name();
        role != Some("user") && role != Some("tool") && content_has_image(&message.content_blocks())
    }) {
        return Err(LlmError::try_new(
            "DeepSeek Messages supports images only in user messages and tool results",
            UNSUPPORTED_CONTENT_CODE,
            None,
            None,
            None,
            None,
        )
        .expect("role fields are static")
        .into());
    }
    let mut versions: HashMap<AttachmentId, RequestImageAttachment> = HashMap::new();
    for message in &messages {
        for block in message.content_blocks() {
            let ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) = block else {
                continue;
            };
            if versions.contains_key(&attachment.attachment_id) {
                continue;
            }
            let target = resolve_request_image_target(model, &attachment);
            let version = attachments.read_image_request(&attachment, target, signal)?;
            versions.insert(attachment.attachment_id, version);
        }
    }
    assert_images_fit(&messages, &versions, connection, ImageRepresentation::Raw)?;
    Ok(PreparedImages { messages, versions })
}

/// Require logged offload before retrying images that exceed the inline
/// budget (dsh `inlineImages`): unchanged history within both byte and
/// image-count limits.
///
/// # Errors
/// [`IMAGE_OFFLOAD_REQUIRED_CODE`] when the inline budget is exceeded.
pub fn inline_images<S: ::std::hash::BuildHasher>(
    messages: &[RequestMessage],
    versions: &HashMap<AttachmentId, RequestImageAttachment, S>,
    connection: &crate::types::ConnectionOptions,
) -> Result<(), LlmError> {
    assert_images_fit(messages, versions, connection, ImageRepresentation::Base64)
}

/// Resolve retained images to Files ids, recording every occurrence for
/// failure diagnostics (dsh `prepareFileIds`).
///
/// # Errors
/// [`FileResolutionFailure`] from the underlying store resolution.
///
/// # Panics
/// Only if a retained image reference has no prepared request version,
/// which [`prepare_images`] establishes for every retained occurrence.
pub fn prepare_file_ids<S: ::std::hash::BuildHasher>(
    messages: &[RequestMessage],
    versions: &HashMap<AttachmentId, RequestImageAttachment, S>,
    files: &mut RequestFiles<'_>,
) -> Result<HashMap<AttachmentId, DeepSeekFileId>, FileResolutionFailure> {
    let mut ids: HashMap<AttachmentId, DeepSeekFileId> = HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        let mut image = 0u64;
        for block in message.content_blocks() {
            let ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) = block else {
                continue;
            };
            image += 1;
            let version = versions
                .get(&attachment.attachment_id)
                .expect("prepare_images prepared every retained reference");
            let location = ImageWireLocation {
                message: u64::try_from(index + 1).unwrap_or(u64::MAX),
                image,
            };
            ids.insert(
                attachment.attachment_id.clone(),
                files.resolve(version, location)?,
            );
        }
    }
    Ok(ids)
}

// Rust guideline compliant 2026-10-26

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ConnectionOptions;
    use fr_llm::adapter::{ImageRequestTarget, ModelModality};
    use fr_wire::message::{KnownMessage, Message, MessageSource};
    use fr_wire::{KnownMessageSource, MessageId, RawObject, TrueOnly};

    fn connection(model: &CatalogFixture) -> ConnectionOptions {
        let entry = crate::models::CatalogModel {
            id: "deepseek-flash".to_owned(),
            name: None,
            description: None,
            context_window: Some(1_000_000),
            max_tokens: None,
            input_modalities: model
                .vision
                .then_some(vec![ModelModality::Text, ModelModality::Image]),
            image_pixel_budget: None,
            image_max_bytes: None,
            system_prompt_update: None,
            tool_update: None,
        };
        ConnectionOptions {
            base_url: "https://api.deepseek.com/anthropic".to_owned(),
            defaults: crate::types::RequestDefaults::default(),
            max_tokens: 4_096,
            default_context_window: 1_000_000,
            models: vec![entry],
            stream_idle_timeout_ms: 300_000.0,
            max_request_files_bytes: 1_024,
            max_inline_request_image_bytes: 64,
            max_images_per_request: 600,
            image_offload_byte_quantum: 1,
            inline_image_offload_byte_quantum: 1,
            image_offload_count_quantum: 1,
            files_api_timeout_ms: 60_000.0,
            file_policy: crate::types::FilePolicy {
                expires_after_seconds: 3_600,
                refresh_margin_seconds: 60,
                quota_cleanup_batch: 10,
            },
            retry_policy: fr_llm::retry_policy::RetryPolicy::Normal {
                max_retries: 5,
                retryable_codes: Vec::new(),
                backoff: fr_llm::retry_policy::Backoff {
                    initial_delay_ms: 500,
                    max_delay_ms: 10_000,
                    jitter_ratio: 0.1,
                },
            },
        }
    }

    /// Catalog shape knob for the fixtures below.
    struct CatalogFixture {
        vision: bool,
    }

    fn reference(id: &str) -> ImageAttachmentRef {
        ImageAttachmentRef {
            attachment_id: AttachmentId::new(format!("sha256:{id}")),
            media_type: fr_wire::ImageMediaType::Png,
            bytes: 1_024,
            width: 320,
            height: 200,
            name: None,
            original_dimensions: None,
        }
    }

    fn image_block(attachment: ImageAttachmentRef) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Image {
            attachment,
            offloaded: None,
        })
    }

    fn offloaded_block(attachment: ImageAttachmentRef) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Image {
            attachment,
            offloaded: Some(TrueOnly),
        })
    }

    fn canned_version(reference: &ImageAttachmentRef) -> RequestImageAttachment {
        RequestImageAttachment {
            variant_id: format!("sha256:{}", "cd".repeat(32)),
            attachment: reference.clone(),
            data: vec![3u8; 512],
            media_type: fr_wire::ImageMediaType::Jpeg,
            bytes: 512,
            width: 320,
            height: 200,
            depth: fr_attachment::request_image::ImageSampleDepth::Uchar,
            space: fr_attachment::request_image::ImageColorSpace::Srgb,
            has_alpha: false,
        }
    }

    /// Attachment seam returning one canned version.
    struct CannedRead {
        version: RequestImageAttachment,
    }

    impl AttachmentRead for CannedRead {
        fn read_image_request(
            &self,
            _reference: &ImageAttachmentRef,
            _target: ImageRequestTarget,
            _signal: Option<&AbortSignal>,
        ) -> Result<RequestImageAttachment, AttachmentError> {
            Ok(self.version.clone())
        }
    }

    fn assistant_with_image(attachment: ImageAttachmentRef) -> RequestMessage {
        RequestMessage::Durable(Message::Known(Box::new(KnownMessage::Assistant {
            id: MessageId::new("msg-1"),
            content: vec![image_block(attachment)],
            source: MessageSource::Known(KnownMessageSource::Model {
                provider: "deepseek-official".to_owned(),
                model: "deepseek-flash".to_owned(),
                replay_state: None,
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        })))
    }

    /// The route gates pin the dsh arms: image-free history passes
    /// through untouched, offloaded occurrences project to text, a
    /// text-only or store-less route refuses, assistant-carried images
    /// refuse, and the vision route prepares one version per attachment.
    #[test]
    fn prepare_images_pins_route_arms() {
        let route = connection(&CatalogFixture { vision: true });
        let access = |_: &ImageAttachmentRef| None;

        // Image-free history passes through with no versions.
        let history = vec![RequestMessage::UserInput {
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: "hello".to_owned(),
            })],
        }];
        let prepared =
            prepare_images(&history, &route, "deepseek-flash", None, &access, None).unwrap();
        assert!(prepared.versions.is_empty());
        assert_eq!(prepared.messages.len(), 1);

        // Offloaded occurrences project to their substitute text.
        let attachment_ref = reference("42");
        let offloaded = offloaded_block(attachment_ref.clone());
        let history = vec![RequestMessage::UserInput {
            content: vec![offloaded],
        }];
        let prepared =
            prepare_images(&history, &route, "deepseek-flash", None, &access, None).unwrap();
        assert!(prepared.versions.is_empty());
        assert!(matches!(
            &prepared.messages[0].content_blocks()[0],
            ContentBlock::Known(KnownContentBlock::Text { text }) if text == &offloaded_image_text(&attachment_ref, None)
        ));

        // A text-only route refuses image input.
        let text_only = connection(&CatalogFixture { vision: false });
        let history = vec![RequestMessage::UserInput {
            content: vec![image_block(attachment_ref.clone())],
        }];
        let Err(error) =
            prepare_images(&history, &text_only, "deepseek-flash", None, &access, None)
        else {
            panic!("text-only route must refuse images");
        };
        assert_eq!(error.code(), UNSUPPORTED_CONTENT_CODE);
        assert_eq!(
            error.message(),
            "DeepSeek Messages image input requires a vision model and attachment service"
        );

        // A vision route without the attachment store refuses likewise.
        let Err(error) = prepare_images(&history, &route, "deepseek-flash", None, &access, None)
        else {
            panic!("store-less route must refuse images");
        };
        assert_eq!(
            error.message(),
            "DeepSeek Messages image input requires a vision model and attachment service"
        );

        // Images outside user/tool messages refuse.
        let history = vec![assistant_with_image(attachment_ref.clone())];
        let Err(error) = prepare_images(
            &history,
            &route,
            "deepseek-flash",
            Some(&CannedRead {
                version: canned_version(&attachment_ref),
            }),
            &access,
            None,
        ) else {
            panic!("assistant-carried images must refuse");
        };
        assert_eq!(
            error.message(),
            "DeepSeek Messages supports images only in user messages and tool results"
        );

        // The happy path prepares one version per retained attachment.
        let history = vec![RequestMessage::UserInput {
            content: vec![
                image_block(attachment_ref.clone()),
                image_block(reference("43")),
            ],
        }];
        let prepared = prepare_images(
            &history,
            &route,
            "deepseek-flash",
            Some(&CannedRead {
                version: canned_version(&attachment_ref),
            }),
            &access,
            None,
        )
        .unwrap();
        assert_eq!(prepared.versions.len(), 2);
        assert!(
            prepared
                .versions
                .contains_key(&attachment_ref.attachment_id)
        );
        assert_eq!(prepared.messages.len(), 1);
    }

    /// The fit checks pin the offload error text, code, and pairing on
    /// both representations.
    #[test]
    fn fit_checks_pin_offload_error_arms() {
        let route = connection(&CatalogFixture { vision: true });
        let attachment_ref = reference("45");
        let mut version = canned_version(&attachment_ref);
        version.bytes = 2_048;
        let mut versions = HashMap::new();
        versions.insert(attachment_ref.attachment_id.clone(), version);
        let history = vec![RequestMessage::UserInput {
            content: vec![image_block(attachment_ref)],
        }];
        let error =
            assert_images_fit(&history, &versions, &route, ImageRepresentation::Raw).unwrap_err();
        assert_eq!(error.code(), IMAGE_OFFLOAD_REQUIRED_CODE);
        assert_eq!(
            error.message(),
            "DeepSeek Messages raw request images exceed the route budget; 1 more oldest occurrence(s) must be offloaded."
        );
        assert_eq!(error.failure().offload_images, Some(1));

        // Base64: the inline budget (64) is far below the base64 length
        // of 2048 bytes, so the same history fails there.
        let Err(error) = inline_images(&history, &versions, &route) else {
            panic!("inline budget must be exceeded");
        };
        assert_eq!(
            error.message(),
            "DeepSeek Messages base64 request images exceed the route budget; 1 more oldest occurrence(s) must be offloaded."
        );
    }

    /// prepareFileIds resolves every occurrence under one-based message
    /// and image positions.
    #[test]
    fn prepare_file_ids_pins_occurrence_locations() {
        let root = std::env::temp_dir().join(format!(
            "fr-images-ids-{}-{}",
            std::process::id(),
            fr_attachment::request_image_pipeline::random_hex_suffix()
        ));
        std::fs::create_dir_all(&root).expect("scratch");
        let fetch = RecordingFetchForIds::default();
        let store = Box::leak(Box::new(crate::file_store::DeepSeekFileStore::new(
            crate::file_store::FileStoreOptions {
                index: crate::upload_index::DeepSeekUploadIndex::new(root.join("files-v3.json")),
                now: &|| 1_000,
                fetch: &fetch,
            },
        )));
        let connection = Box::leak(Box::new(crate::file_store::FileConnection {
            base_url: "https://api.deepseek.com/anthropic".to_owned(),
            headers: vec![("x-api-key".to_owned(), "k".to_owned())],
        }));
        let policy = Box::leak(Box::new(crate::types::FilePolicy {
            expires_after_seconds: 3_600,
            refresh_margin_seconds: 60,
            quota_cleanup_batch: 10,
        }));
        let activity = || {};
        let mut files = RequestFiles::new(store, connection, policy, 5_000.0, None, &activity);

        let first = reference("46");
        let second = reference("47");
        let mut versions = HashMap::new();
        versions.insert(first.attachment_id.clone(), canned_version(&first));
        versions.insert(second.attachment_id.clone(), canned_version(&second));
        let messages = vec![
            RequestMessage::UserInput {
                content: vec![image_block(first.clone())],
            },
            RequestMessage::UserInput {
                content: vec![image_block(second)],
            },
        ];
        let ids = prepare_file_ids(&messages, &versions, &mut files).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(
            ids.get(&first.attachment_id)
                .map(crate::file_id::DeepSeekFileId::as_str),
            Some("file-up")
        );
        // Both occurrences resolved through the store (two uploads).
        assert_eq!(fetch.uploads.load(std::sync::atomic::Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Minimal Files transport double for the id-resolution test.
    struct RecordingFetchForIds {
        uploads: std::sync::atomic::AtomicUsize,
    }

    impl Default for RecordingFetchForIds {
        fn default() -> Self {
            Self {
                uploads: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl fr_net::http::Fetch for RecordingFetchForIds {
        fn fetch(
            &self,
            request: fr_net::http::HttpRequest,
        ) -> Result<fr_net::http::HttpResponse, fr_net::http::TransportError> {
            let send = |status: u16, body: String| {
                Ok(fr_net::http::HttpResponse {
                    status,
                    headers: Vec::new(),
                    body: Box::new(std::io::Cursor::new(body.into_bytes())),
                })
            };
            if request.url.ends_with("/v1/files")
                && matches!(request.method, fr_net::http::HttpMethod::Post)
            {
                self.uploads
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body = request.body.expect("upload body");
                let text = String::from_utf8_lossy(&body);
                let start = text.rfind("\r\n\r\n").expect("file part") + 4;
                let end = text[start..].find("\r\n--").expect("terminator") + start;
                return send(
                    200,
                    format!(
                        "{{\"id\":\"file-up\",\"type\":\"file\",\"mime_type\":\"image/png\",\
                         \"size_bytes\":{},\"created_at\":\"2026-05-01T12:00:00Z\",\
                         \"filename\":\"dsh-up.png\"}}",
                        end - start
                    ),
                );
            }
            send(200, "{\"data\":[],\"has_more\":false}".to_owned())
        }
    }
}

// Rust guideline compliant 2026-10-26
