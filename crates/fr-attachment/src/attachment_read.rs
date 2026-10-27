//! The attachment-store seam (Q71-B; 2026-10-27 Q78-A contract home).
//!
//! The attachment domain owns the service face, mirroring dsh's
//! independent `dsh-attachment` package whose store the provider adapters
//! reach. The adapter consumes this trait through its injected seam; the
//! production implementation is the fr-attachment service over the
//! pipeline, and tests inject fixtures.

use crate::request_image::RequestImageAttachment;
use crate::request_image_pipeline::AttachmentError;
use fr_llm::adapter::{AbortSignal, ImageRequestTarget};
use fr_wire::ImageAttachmentRef;

/// The attachment-store seam: reads one route-chosen request image for a
/// retained reference.
pub trait AttachmentRead {
    /// Read or produce one deterministic request image under the target.
    ///
    /// # Errors
    /// [`AttachmentError`] for malformed inputs, failed normalization, or
    /// caller cancellation.
    fn read_image_request(
        &self,
        reference: &ImageAttachmentRef,
        target: ImageRequestTarget,
        signal: Option<&AbortSignal>,
    ) -> Result<RequestImageAttachment, AttachmentError>;
}

// Rust guideline compliant 2026-10-27
