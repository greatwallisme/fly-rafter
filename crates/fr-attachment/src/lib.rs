//! The fr-attachment facility plugin (2026-10-27 Q78-A): the attachment
//! domain one home, mirroring dsh's independent `dsh-attachment` package.
//!
//! The contract face — the [`AttachmentRead`] seam, [`request_image::RequestImageAttachment`],
//! and the deterministic request-image normalization pipeline — is
//! self-held on the plugin's rlib. The dylib artifact provides the
//! `attachments` service over the dougong dynamic channel; wiring that
//! service into each adapter's injected seam belongs to the
//! runtime-registry slice.

pub mod admission;
pub mod attachment_read;
pub mod error;
pub mod file_store;
pub mod limits;
pub mod local;
pub mod normalize;
pub mod plugin_shell;
pub mod request_image;
pub mod request_image_pipeline;
pub mod store;

pub use attachment_read::AttachmentRead;
pub use request_image_pipeline::AttachmentError;

// Rust guideline compliant 2026-10-27
