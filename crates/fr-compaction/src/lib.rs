//! The context compaction crate (dsh `packages/compaction` 1:1): the
//! compaction service vocabulary, the checkpoint message source, tool-pairing
//! balance, and the compaction-basic config resolution family.
//!
//! Crate layout mirrors the dsh packages one module per file:
//!
//! - [`brand`], [`types`], [`checkpoint`], and [`tool_pairing`] port dsh
//!   `packages/compaction/compaction/src/` (brand.ts, types.ts + index.ts,
//!   checkpoint.ts, tool-pairing.ts).
//! - [`config`] ports dsh
//!   `packages/compaction/compaction-basic/src/config.ts` (load-time
//!   validation, routed-model policy merge, and capacity scaling with the
//!   `TargetPressureConfigError` classification).
//!
//! Documented structurals (behavior-identical, not protocol or output
//! deltas), following the token-meter crate judgment:
//!
//! - dsh `CompactionEngine` is an abstract class published as the
//!   `ctx.compaction` service. fr publishes the same entries as the plugin's
//!   provided service face, assembled with the engine shell at 17f; the
//!   three abstract entries' call vocabulary
//!   ([`types::CompactionTrigger`], [`types::ManualCompactionError`],
//!   [`types::CompactionResult`]) is complete here.
//! - dsh deep-freezes resolved config (`deepFreeze`); fr resolves into
//!   plain owned structs consumed by shared reference — same observable
//!   immutability, no runtime freeze assertion exists on the dsh side to
//!   mirror.
//! - dsh keys the tool-pairing balance cache by live `Session` object in a
//!   `WeakMap`; fr keys by session id in a map owned by the engine shell
//!   (17e/f). Eviction only forces a from-scratch rebuild, and the answers
//!   stay identical.
//! - dsh `CommandId` is a branded string from `dsh-commands/brand`; the
//!   fr-commands crate is not built in M1, so the event payloads carry the
//!   same wire shape as a plain `String` (both sides validate nothing about
//!   the value).
//! - dsh `SessionSeq` numbers carry no arithmetic discipline at this layer;
//!   fr models them as `u64` (`fr_wire::SessionSeq`), so the shadow-event
//!   payload structs serialize the same wire shapes.

pub mod brand;
pub mod checkpoint;
pub mod config;
pub mod engine;
pub mod image_offload;
pub mod llm_face;
pub mod meter;
pub mod plugin_shell;
pub mod prompts;
pub mod pruner;
pub mod region;
pub mod session_view;
pub mod summarizer;
pub mod tool_pairing;
pub mod types;
