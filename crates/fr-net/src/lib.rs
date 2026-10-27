//! The fr-net facility plugin (2026-10-27 Q77-A): the HTTP transport
//! service every provider adapter consumes.
//!
//! The contract face ([`http::Fetch`]/[`http::HttpRequest`]/[`http::HttpResponse`]
//! /[`http::TransportError`]/[`http::HttpMethod`]) is self-held on the
//! plugin's rlib; the production backend is the workspace's single `ureq`
//! site (Q69-B carrier: rustls default, redirects refused, read-window
//! abort granularity). The dylib artifact provides the `net` service over
//! the dougong dynamic channel; wiring that service into each adapter's
//! injected seam belongs to the runtime-registry slice.

pub mod http;
pub mod plugin_shell;

// Rust guideline compliant 2026-10-27
