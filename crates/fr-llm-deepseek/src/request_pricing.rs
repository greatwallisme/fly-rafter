//! Provider-side request-image pricing for `DeepSeek` routes (dsh
//! `request-pricing.ts`).
//!
//! Prices every retained surface occurrence at its
//! per-model request target with the published vision-token accounting,
//! and every occurrence the surface marks offloaded as its substitute
//! text.
//!
//! Consumed synchronously through
//! [`fr_llm::adapter::ImageRequestPricing`]; provider usage remains the
//! authoritative anchor for completed requests.

use crate::image_tokens::{deep_seek_image_tokens, deep_seek_request_image_dimensions};
use crate::models::{CatalogModel, ImagePixelBudget};
use crate::types::ConnectionOptions;
use fr_llm::adapter::{ImageRequestPrice, ImageRequestPricing, ImageRequestTarget, ModelModality};
use fr_llm::projection::{
    ImageAttachmentAccess, ProjectedDimensions, offloaded_image_text, request_image_dimensions,
    request_image_handle_text, text_only_image_text,
};
use fr_wire::{ContentBlock, ImageAttachmentRef, KnownContentBlock};

/// Default bound on accumulated file-referenced image bytes per request
/// (dsh `DEFAULT_MAX_REQUEST_FILES_BYTES`).
pub const DEFAULT_MAX_REQUEST_FILES_BYTES: u64 = 128 * 1024 * 1024;
/// Provider request image-count limit (dsh `DEFAULT_MAX_IMAGES_PER_REQUEST`).
pub const DEFAULT_MAX_IMAGES_PER_REQUEST: u64 = 600;
/// Total-pixel budget matching provider low-detail image input (dsh
/// `DEFAULT_LOW_DETAIL_IMAGE_PIXEL_BUDGET`).
pub const DEFAULT_LOW_DETAIL_IMAGE_PIXEL_BUDGET: u64 = 512 * 512;
/// Encoded-byte target for one deterministic model-request image; the
/// smallest quality-ladder output is used when no quality fits (dsh
/// `DEFAULT_REQUEST_IMAGE_MAX_BYTES`).
pub const DEFAULT_REQUEST_IMAGE_MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Provider per-side limit for a request carrying 15 or more images,
/// applied to every request image so the image count never changes a
/// projection (dsh `REQUEST_IMAGE_MAX_DIMENSION`).
pub const REQUEST_IMAGE_MAX_DIMENSION: u32 = 4096;

/// Resolve the encoded-byte target one `DeepSeek` model route applies to
/// every request image (dsh `resolveRequestImageMaxBytes`).
#[must_use]
pub fn resolve_request_image_max_bytes(model: &CatalogModel) -> u64 {
    model
        .image_max_bytes
        .unwrap_or(DEFAULT_REQUEST_IMAGE_MAX_BYTES)
}

/// Resolve the deterministic request target one `DeepSeek` model route
/// chooses for one source image (dsh `resolveRequestImageTarget`).
///
/// The
/// published token grid unless the model overrides it with a pixel
/// budget, then the provider per-side limit, then the route's
/// encoded-byte target. Small images are never enlarged.
#[must_use]
pub fn resolve_request_image_target(
    model: &CatalogModel,
    source: &ImageAttachmentRef,
) -> ImageRequestTarget {
    let projected: ProjectedDimensions = match model.image_pixel_budget {
        Some(ImagePixelBudget::Low) => request_image_dimensions(
            source.width,
            source.height,
            DEFAULT_LOW_DETAIL_IMAGE_PIXEL_BUDGET,
        ),
        Some(ImagePixelBudget::Pixels(budget)) => {
            request_image_dimensions(source.width, source.height, budget)
        }
        None => deep_seek_request_image_dimensions(source.width, source.height),
    };
    let capped = if projected.width.max(projected.height) > REQUEST_IMAGE_MAX_DIMENSION {
        long_edge_or_source(source, REQUEST_IMAGE_MAX_DIMENSION)
    } else {
        projected
    };
    ImageRequestTarget {
        width: capped.width,
        height: capped.height,
        max_bytes: resolve_request_image_max_bytes(model),
    }
}

fn long_edge_or_source(source: &ImageAttachmentRef, long_edge: u32) -> ProjectedDimensions {
    fr_llm::projection::long_edge_dimensions(source.width, source.height, long_edge)
}

/// Current execution-world access resolution shared with request
/// serialization (dsh `ImageAttachmentAccessResolver`).
pub type AccessResolver<'a> =
    &'a (dyn Fn(&ImageAttachmentRef) -> Option<ImageAttachmentAccess> + Sync);

/// The request-image pricing of one `DeepSeek` route (dsh
/// `deepSeekImageRequestPricing`'s returned object).
///
/// Uncatalogued and
/// text-only models price every occurrence as its deterministic text
/// substitution; image-capable models price an offloaded occurrence as
/// its substitute text and a retained one by its projected request
/// dimensions, with each occurrence's handle or substitute text built
/// through the same access resolution the serializer uses. Access paths
/// resolve at pricing time, so a path that changes before the request
/// only shifts the text price by its own length.
pub struct DeepSeekImageRequestPricing<'a> {
    catalog_model: Option<CatalogModel>,
    resolve_access: Option<AccessResolver<'a>>,
}

impl std::fmt::Debug for DeepSeekImageRequestPricing<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepSeekImageRequestPricing")
            .field("catalog_model", &self.catalog_model)
            .finish_non_exhaustive()
    }
}

/// Build the request-image pricing for one `DeepSeek` route from a
/// validated connection snapshot (dsh `deepSeekImageRequestPricing`).
#[must_use]
pub fn deep_seek_image_request_pricing<'a>(
    connection: &ConnectionOptions,
    model: &str,
    resolve_access: Option<AccessResolver<'a>>,
) -> DeepSeekImageRequestPricing<'a> {
    let catalog_model = connection
        .model(model)
        .filter(|entry| {
            entry
                .input_modalities
                .as_ref()
                .is_some_and(|modalities| modalities.contains(&ModelModality::Image))
        })
        .cloned();
    DeepSeekImageRequestPricing {
        catalog_model,
        resolve_access,
    }
}

impl ImageRequestPricing for DeepSeekImageRequestPricing<'_> {
    /// Price every image occurrence of one request projection, aligned by
    /// index with the image blocks; non-image blocks are ignored because
    /// the runtime hands the pricing only image occurrences.
    fn price_images(&self, images: &[ContentBlock]) -> Vec<ImageRequestPrice> {
        images
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Known(KnownContentBlock::Image {
                    attachment,
                    offloaded,
                }) => Some((attachment, offloaded)),
                _ => None,
            })
            .map(|(reference, offloaded)| {
                let Some(catalog_model) = &self.catalog_model else {
                    return ImageRequestPrice {
                        visual_tokens: 0,
                        text: text_only_image_text(reference),
                    };
                };
                if offloaded.is_some() {
                    return ImageRequestPrice {
                        visual_tokens: 0,
                        text: offloaded_image_text(reference, self.access(reference).as_ref()),
                    };
                }
                let target = resolve_request_image_target(catalog_model, reference);
                ImageRequestPrice {
                    visual_tokens: deep_seek_image_tokens(target.width, target.height),
                    text: request_image_handle_text(
                        reference,
                        (target.width, target.height),
                        self.access(reference).as_ref(),
                    ),
                }
            })
            .collect()
    }
}

impl DeepSeekImageRequestPricing<'_> {
    fn access(&self, reference: &ImageAttachmentRef) -> Option<ImageAttachmentAccess> {
        self.resolve_access.and_then(|resolve| resolve(reference))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::ImageMediaType;

    fn connection_with(model: CatalogModel) -> ConnectionOptions {
        ConnectionOptions {
            base_url: "https://api.deepseek.com/anthropic".to_owned(),
            defaults: crate::types::RequestDefaults::default(),
            max_tokens: 4_096,
            default_context_window: 1_000_000,
            models: vec![model],
            stream_idle_timeout_ms: 300_000.0,
            max_request_files_bytes: DEFAULT_MAX_REQUEST_FILES_BYTES,
            max_inline_request_image_bytes: 32 * 1024 * 1024,
            max_images_per_request: DEFAULT_MAX_IMAGES_PER_REQUEST,
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

    fn vision_model() -> CatalogModel {
        CatalogModel {
            id: "deepseek-flash".to_owned(),
            name: None,
            description: None,
            context_window: Some(1_000_000),
            max_tokens: None,
            input_modalities: Some(vec![ModelModality::Text, ModelModality::Image]),
            image_pixel_budget: None,
            image_max_bytes: None,
            system_prompt_update: None,
            tool_update: None,
        }
    }

    fn reference(width: u32, height: u32) -> ImageAttachmentRef {
        ImageAttachmentRef {
            attachment_id: fr_wire::ids::AttachmentId::new(format!("sha256:{}", "42".repeat(32))),
            media_type: ImageMediaType::Png,
            bytes: 1_024,
            width,
            height,
            name: None,
            original_dimensions: None,
        }
    }

    fn image_block(attachment: ImageAttachmentRef, offloaded: bool) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Image {
            attachment,
            offloaded: offloaded.then_some(fr_wire::TrueOnly),
        })
    }

    /// The five route constants pin the dsh values.
    #[test]
    fn pricing_constants_pin_dsh_values() {
        assert_eq!(DEFAULT_MAX_REQUEST_FILES_BYTES, 134_217_728);
        assert_eq!(DEFAULT_MAX_IMAGES_PER_REQUEST, 600);
        assert_eq!(DEFAULT_LOW_DETAIL_IMAGE_PIXEL_BUDGET, 262_144);
        assert_eq!(DEFAULT_REQUEST_IMAGE_MAX_BYTES, 2_097_152);
        assert_eq!(REQUEST_IMAGE_MAX_DIMENSION, 4096);
    }

    /// The route target follows the published token grid by default, the
    /// model pixel-budget override when present, the provider per-side cap
    /// beyond 4096, and the model's byte override for maxBytes.
    #[test]
    fn resolve_target_pins_budget_layers() {
        let model = vision_model();
        // Default: published token grid for an oversized square.
        let target = resolve_request_image_target(&model, &reference(4096, 4096));
        assert_eq!((target.width, target.height), (1302, 1302));
        assert_eq!(target.max_bytes, DEFAULT_REQUEST_IMAGE_MAX_BYTES);

        // Small images are never enlarged.
        let small = resolve_request_image_target(&model, &reference(320, 200));
        assert_eq!((small.width, small.height), (320, 200));

        // The low preset replaces the token grid.
        let low = CatalogModel {
            image_pixel_budget: Some(ImagePixelBudget::Low),
            ..vision_model()
        };
        let target = resolve_request_image_target(&low, &reference(800, 600));
        assert_eq!((target.width, target.height), (591, 443));

        // The provider per-side cap applies when a projection exceeds
        // 4096 (a huge pixel budget keeps the source dimensions).
        let oversized = CatalogModel {
            image_pixel_budget: Some(ImagePixelBudget::Pixels(200_000_000)),
            ..vision_model()
        };
        let target = resolve_request_image_target(&oversized, &reference(8000, 6000));
        assert_eq!((target.width, target.height), (4096, 3072));

        // The model byte override flows into maxBytes.
        let bytes = CatalogModel {
            image_max_bytes: Some(512),
            ..vision_model()
        };
        assert_eq!(resolve_request_image_max_bytes(&bytes), 512);
    }

    /// Text-only routes price every occurrence as its text substitution;
    /// vision routes price offloaded occurrences as substitute text and
    /// retained ones by token count plus handle text.
    #[test]
    fn price_images_pins_route_arms() {
        let connection = connection_with(vision_model());
        let pricing = deep_seek_image_request_pricing(&connection, "deepseek-flash", None);
        let block = image_block(reference(4096, 4096), false);
        let prices = pricing.price_images(&[block]);
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0].visual_tokens, 994);
        assert_eq!(
            prices[0].text,
            fr_llm::projection::request_image_handle_text(
                &reference(4096, 4096),
                (1302, 1302),
                None
            )
        );

        // Offloaded occurrences price as substitute text.
        let offloaded = image_block(reference(4096, 4096), true);
        let prices = pricing.price_images(&[offloaded]);
        assert_eq!(prices[0].visual_tokens, 0);
        assert_eq!(
            prices[0].text,
            fr_llm::projection::offloaded_image_text(&reference(4096, 4096), None)
        );

        // Non-image blocks are ignored.
        let mixed = vec![
            ContentBlock::Known(KnownContentBlock::Text {
                text: "hi".to_owned(),
            }),
            image_block(reference(4096, 4096), false),
        ];
        assert_eq!(pricing.price_images(&mixed).len(), 1);

        // A text-only route substitutes every occurrence.
        let text_only = connection_with(CatalogModel {
            input_modalities: None,
            ..vision_model()
        });
        let pricing = deep_seek_image_request_pricing(&text_only, "deepseek-flash", None);
        let prices = pricing.price_images(&[image_block(reference(4096, 4096), false)]);
        assert_eq!(prices[0].visual_tokens, 0);
        assert_eq!(
            prices[0].text,
            fr_llm::projection::text_only_image_text(&reference(4096, 4096))
        );
    }
}

// Rust guideline compliant 2026-10-26
