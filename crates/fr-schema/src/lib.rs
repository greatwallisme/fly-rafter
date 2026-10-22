//! Plugin configuration schemas for Fly Rafter (PRD 6.3).
//!
//! Declarative descriptor trees are the single source of truth: one
//! declaration derives both validation ([`validate`]) and the JSON Schema
//! 2020-12 projection ([`to_json_schema`]), mirroring dsh's schemastery
//! objects that feed validation and dump projection alike (2026-10-22 Q7
//! ruling). The registry ([`lookup`]) maps first-party plugin slugs to
//! their declarations; plugin artifacts link this same crate to
//! re-validate config at activation (dual-side validation, PRD 6.3).

#![forbid(unsafe_code)]

pub mod registry;
pub mod schema;

pub use registry::lookup;
pub use schema::{Property, Schema, Violation, to_json_schema, validate};
