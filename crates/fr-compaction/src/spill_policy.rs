//! The spill policy (the dsh `@deepseek-ai/dsh-spill-policy` 1:1):
//! token-budgeted tool content with recoverable addresses.
//!
//! Post-execute policies settle before retention; canonical program
//! values remain intact. Missing recovery storage or image pricing keeps
//! the original content and reports the reason through the logger.
//!
//! The pure half lives here (the retention math and the notice
//! vocabulary); the endpoint orchestration that reads the wire seams
//! lives in the plugin shell (the 17f seam split).

/// One content block the policy can retain: text splits, images stay
/// whole (the dsh `RetainableBlock` gate — every block must be text or
/// image; anything else declines the whole bound).
pub type RetainableContent = Vec<fr_wire::ContentBlock>;

/// Retained ends and exact quantities excluded from their original
/// sequence (the dsh `RetainedContent`).
#[derive(Debug, Clone, PartialEq)]
pub struct RetainedContent {
    /// The retained head blocks, in original order.
    pub head: Vec<fr_wire::ContentBlock>,
    /// The retained tail blocks, in original order.
    pub tail: Vec<fr_wire::ContentBlock>,
    /// UTF-8 bytes of text omitted between the ends.
    pub omitted_bytes: u64,
    /// Whole images omitted between the ends.
    pub omitted_images: u64,
}

/// The fixed gap marker between the retained ends (the dsh `GAP`).
pub const GAP_TEXT: &str = "\n\n[...]\n\n";

/// The framing text ahead of the footer notice (the dsh
/// `` `\n\n${footer}` `` composition).
pub const FOOTER_SEPARATOR: &str = "\n\n";

/// Preserves a UTF-16 code point when a text budget cuts between its
/// surrogates (the dsh `textSlice`; the cut walks back one code unit
/// when it lands between a high and a low surrogate).
#[must_use]
pub fn text_slice(text: &str, length: usize, tail: bool) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let total = units.len();
    let mut cut = if tail {
        total.saturating_sub(length)
    } else {
        length.min(total)
    };
    let previous = cut.checked_sub(1).map(|index| units[index]);
    let current = units.get(cut).copied();
    if let (Some(previous), Some(current)) = (previous, current)
        && (0xd800..=0xdbff).contains(&previous)
        && (0xdc00..=0xdfff).contains(&current)
    {
        cut = if tail { cut + 1 } else { cut - 1 };
    }
    let units = if tail { &units[cut..] } else { &units[..cut] };
    // A Rust `str` carries no unpaired surrogates, so every surrogate-safe
    // slice round-trips exactly (the dsh JS strings hold the same fact).
    String::from_utf16(units).unwrap_or_default()
}

/// Largest contiguous text end fitting a monotonic token estimate (the
/// dsh `fitText` binary search; `ceil` midpoint, an empty or over-budget
/// candidate moves the upper bound down).
#[must_use]
pub fn fit_text(
    text: &str,
    budget: u64,
    tail: bool,
    price: &dyn Fn(&fr_wire::ContentBlock) -> u64,
) -> String {
    let total = text.encode_utf16().count();
    let mut low = 0_usize;
    let mut high = total;
    while low < high {
        let middle = (low + high).div_ceil(2);
        let candidate = text_slice(text, middle, tail);
        let candidate_price = price(&text_block(&candidate));
        if candidate.is_empty() || candidate_price <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    text_slice(text, low, tail)
}

/// Builds one text content block.
#[must_use]
pub fn text_block(text: &str) -> fr_wire::ContentBlock {
    fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Text {
        text: text.to_owned(),
    })
}

/// Retain contiguous ends without moving or partially retaining an image
/// (the dsh `retainContent`). Each end receives half the content budget;
/// unused space at an indivisible image stays unused.
#[must_use]
pub fn retain_content(
    content: &[fr_wire::ContentBlock],
    budget: u64,
    price: &dyn Fn(&fr_wire::ContentBlock) -> u64,
) -> RetainedContent {
    let mut head: Vec<fr_wire::ContentBlock> = Vec::new();
    let mut tail: Vec<fr_wire::ContentBlock> = Vec::new();
    let mut first = 0_usize;
    let mut last = content.len().saturating_sub(1);
    let mut head_characters = 0_usize;
    let mut remaining = budget.div_ceil(2);
    while first <= last {
        let block = &content[first];
        let cost = price(block);
        if cost <= remaining {
            head.push(block.clone());
            remaining -= cost;
            first += 1;
            continue;
        }
        if let Some(text) = block_text(block) {
            let fitted = fit_text(text, remaining, false, price);
            // The head/tail split of a shared block cuts at the fitted
            // head's UTF-16 code-unit length (the dsh `text.length`
            // slice index), not its char count.
            head_characters = fitted.encode_utf16().count();
            if !fitted.is_empty() {
                head.push(text_block(&fitted));
            }
        }
        break;
    }
    let mut remaining = budget / 2;
    loop {
        let original = &content[last];
        let owned;
        let block: &fr_wire::ContentBlock = if last == first && block_text(original).is_some() {
            let text = block_text(original).unwrap_or_default();
            let rest = text_slice(text, head_characters, true);
            owned = text_block(&rest);
            &owned
        } else {
            original
        };
        let cost = price(block);
        if cost <= remaining {
            tail.push(block.clone());
            remaining -= cost;
            if last == 0 {
                break;
            }
            last -= 1;
            if last < first {
                break;
            }
            continue;
        }
        if let Some(text) = block_text(block) {
            let fitted = fit_text(text, remaining, true, price);
            if !fitted.is_empty() {
                tail.push(text_block(&fitted));
            }
        }
        break;
    }
    tail.reverse();
    let bytes = |blocks: &[fr_wire::ContentBlock]| -> u64 {
        blocks
            .iter()
            .map(|block| block_text(block).map_or(0, str::len) as u64)
            .sum()
    };
    let images = |blocks: &[fr_wire::ContentBlock]| -> u64 {
        blocks.iter().filter(|block| is_image(block)).count() as u64
    };
    let omitted_bytes = bytes(content) - bytes(&head) - bytes(&tail);
    let omitted_images = images(content) - images(&head) - images(&tail);
    RetainedContent {
        head,
        tail,
        omitted_bytes,
        omitted_images,
    }
}

/// The text payload of a text block, else `None` (the dsh block-type
/// read).
#[must_use]
pub fn block_text(block: &fr_wire::ContentBlock) -> Option<&str> {
    match block {
        fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Text { text }) => Some(text),
        _ => None,
    }
}

/// Whether one block is an image occurrence.
#[must_use]
pub const fn is_image(block: &fr_wire::ContentBlock) -> bool {
    image_attachment(block).is_some()
}

/// The durable image reference of an image block, else `None`.
#[must_use]
pub const fn image_attachment(
    block: &fr_wire::ContentBlock,
) -> Option<&fr_wire::ImageAttachmentRef> {
    match block {
        fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Image { attachment, .. }) => {
            Some(attachment)
        }
        _ => None,
    }
}

/// The opaque storage identifier of one durable image reference (the dsh
/// `ref.attachmentId`).
#[must_use]
pub fn image_attachment_id(reference: &fr_wire::ImageAttachmentRef) -> &str {
    reference.attachment_id.as_str()
}

/// Whether the whole content is retainable: every block is text or image
/// (the dsh `retainable` gate; anything else declines the bound).
#[must_use]
pub fn retainable(content: &[fr_wire::ContentBlock]) -> bool {
    content
        .iter()
        .all(|block| block_text(block).is_some() || is_image(block))
}

/// The exact-omission clause (the dsh `describeOmitted` exact arm).
#[must_use]
fn describe_omitted_bytes(count: u64) -> String {
    format!("Omitted {count} bytes.")
}

/// Formats the notice appended to a retained preview (the dsh
/// `formatSpillNotice`), preserving its persisted spelling:
///
/// `(Omitted N bytes.[ Omitted M images.] Full formatted result stored
/// at: <locator>. <retrievalHint>)`.
#[must_use]
pub fn format_spill_notice(
    omitted_bytes: u64,
    locator: &str,
    retrieval_hint: &str,
    images: u64,
) -> String {
    const OPEN: &str = "(";
    const CLOSE: &str = ")";
    const LOCATION: &str = " Full formatted result stored at: ";
    const GUIDANCE_SEPARATOR: &str = ". ";
    let image_notice = if images > 0 {
        format!(" Omitted {images} images.")
    } else {
        String::new()
    };
    format!(
        "{OPEN}{omitted}{image_notice}{LOCATION}{locator}{GUIDANCE_SEPARATOR}{retrieval_hint}{CLOSE}",
        omitted = describe_omitted_bytes(omitted_bytes),
    )
}

/// Sums the UTF-8 byte length of every text block (the dsh
/// `Buffer.byteLength(text, 'utf8')` reduce).
#[must_use]
pub fn total_text_bytes(content: &[fr_wire::ContentBlock]) -> u64 {
    content
        .iter()
        .map(|block| block_text(block).map_or(0, str::len) as u64)
        .sum()
}

/// Merges adjacent text blocks in place (the dsh final pass: adjacent
/// text has one framing cost on the wire and in the spill preview).
///
/// # Panics
///
/// Panics when a parsed text block vanishes between the type check and
/// the merge (a structural impossibility — the block was just matched).
#[must_use]
pub fn merge_adjacent_text(content: Vec<fr_wire::ContentBlock>) -> Vec<fr_wire::ContentBlock> {
    let mut merged: Vec<fr_wire::ContentBlock> = Vec::with_capacity(content.len());
    for block in content {
        // Adjacent TEXT merges; a non-text block keeps its own framing.
        let both_text =
            merged.last().and_then(block_text).is_some() && block_text(&block).is_some();
        if both_text {
            let last = merged.last_mut().expect("just checked");
            let last_text = block_text(last).map(str::to_owned).unwrap_or_default();
            *last = text_block(&format!(
                "{last_text}{}",
                block_text(&block).unwrap_or_default()
            ));
        } else {
            merged.push(block);
        }
    }
    merged
}

/// The route price of one image occurrence (the fr-wire pricing reply
/// mirror; the dsh `LlmImageRequestPrice`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePrice {
    /// Provider visual tokens for the retained occurrence.
    pub visual_tokens: u64,
    /// The model-visible text sent for this occurrence.
    pub text: String,
}

/// The spill-policy seams (the dsh `ctx` faces the policy reads). The
/// wire-backed impl lives in the plugin shell; tests drive doubles.
pub trait SpillSeams {
    /// The fixed-estimator price of one block list (the dsh
    /// `estimateContent` import).
    ///
    /// # Errors
    ///
    /// The estimator transport failure.
    fn estimate(&self, blocks: &[fr_wire::ContentBlock]) -> Result<u64, String>;

    /// The route's per-occurrence image prices, aligned with `images`
    /// (the dsh `imageRequestPricing().priceImages` pair).
    ///
    /// # Errors
    ///
    /// The pricing transport failure.
    fn price_images(
        &self,
        provider: &str,
        model: &str,
        images: &[fr_wire::ContentBlock],
    ) -> Result<Vec<ImagePrice>, String>;

    /// The execution-world read path of one durable image reference
    /// (the dsh `resolveImageAttachmentAccess` pair); `None` keeps the
    /// unreadable-refusal arm.
    fn image_access(&self, reference: &fr_wire::ImageAttachmentRef) -> Option<String>;

    /// The routed request-header route (the dsh
    /// `session.requestHeader()?.config`).
    ///
    /// # Errors
    ///
    /// The transport failure.
    fn request_header_route(&self, session_id: &str) -> Result<Option<(String, String)>, String>;

    /// The agent options route (the dsh `agent.options`).
    ///
    /// # Errors
    ///
    /// The transport failure.
    fn agent_route(&self, session_id: &str) -> Result<Option<(String, String)>, String>;

    /// Persists one complete formatted result (the dsh `spillStore`).
    ///
    /// # Errors
    ///
    /// The storage failure (best-effort degrade).
    fn save_text(
        &self,
        save: &crate::spill::SaveTextSpill,
    ) -> Result<crate::spill::SpillRef, String>;
}

/// The no-route refusal (the dsh verbatim text; the calculator lookup
/// answers `undefined`).
#[must_use]
pub fn no_image_calculator() -> String {
    "the current model has no image token calculator".to_owned()
}

/// The occurrence-count mismatch refusal (the dsh verbatim text).
fn inconsistent_occurrence_count() -> String {
    "image token calculator returned an inconsistent occurrence count".to_owned()
}

/// The unreadable-image refusal (the dsh verbatim text; the attachment
/// id interpolates).
fn unreadable_attachment(attachment_id: &str) -> String {
    format!("image {attachment_id} has no readable attachment path")
}

/// The per-block model-route cost the retention math prices against.
type PriceFn<'a> = Box<dyn Fn(&fr_wire::ContentBlock) -> u64 + 'a>;

/// Builds the per-block price closure (the dsh `pricing`): text blocks
/// price under the fixed estimator; each image prices at its stored cost
/// (visual tokens plus the estimator price of the model-visible text).
/// The dsh object-identity cost map becomes value matching (two
/// structurally equal occurrences carry equal costs by construction).
fn build_price<'a>(
    seams: &'a dyn SpillSeams,
    images: &'a [fr_wire::ContentBlock],
    route: Option<&'a (String, String)>,
) -> Result<PriceFn<'a>, String> {
    let image_costs: Vec<u64> = if images.is_empty() {
        Vec::new()
    } else {
        let (provider, model) = route.ok_or_else(no_image_calculator)?;
        let prices = seams.price_images(provider, model, images)?;
        if prices.len() != images.len() {
            return Err(inconsistent_occurrence_count());
        }
        let mut costs = Vec::with_capacity(prices.len());
        for price in prices {
            let text_cost = seams.estimate(&[text_block(&price.text)])?;
            costs.push(price.visual_tokens + text_cost);
        }
        costs
    };
    Ok(Box::new(move |block: &fr_wire::ContentBlock| -> u64 {
        if is_image(block) {
            let ordinal = images
                .iter()
                .position(|candidate| candidate == block)
                .unwrap_or_default();
            image_costs.get(ordinal).copied().unwrap_or_default()
        } else {
            seams
                .estimate(std::slice::from_ref(block))
                .unwrap_or_default()
        }
    }))
}

/// Resolves the pricing route (the dsh `routed = requestHeader()?.config
/// ?? agent.options` pair): the routed request-header route wins, the
/// agent options route fills in.
fn resolve_route(
    seams: &dyn SpillSeams,
    session_id: &str,
) -> Result<Option<(String, String)>, String> {
    if let Some(route) = seams.request_header_route(session_id)? {
        return Ok(Some(route));
    }
    seams.agent_route(session_id)
}

/// Renders the full ordered text with an execution-readable attachment
/// path at each image position (the dsh `fullText`).
fn full_text(seams: &dyn SpillSeams, content: &[fr_wire::ContentBlock]) -> Result<String, String> {
    let mut rendered = String::new();
    for block in content {
        if let Some(text) = block_text(block) {
            rendered.push_str(text);
            continue;
        }
        let Some(reference) = image_attachment(block) else {
            continue;
        };
        let Some(path) = seams.image_access(reference) else {
            return Err(unreadable_attachment(image_attachment_id(reference)));
        };
        let path_text =
            serde_json::to_string(&path).map_err(|error| format!("path encode failed: {error}"))?;
        let media_type = serde_json::to_value(reference.media_type)
            .map_err(|error| format!("media type encode failed: {error}"))?;
        rendered.push_str("\n[Image: ");
        rendered.push_str(&path_text);
        rendered.push_str("; ");
        rendered.push_str(&media_type.to_string());
        rendered.push_str("; ");
        rendered.push_str(&reference.width.to_string());
        rendered.push('x');
        rendered.push_str(&reference.height.to_string());
        rendered.push_str(". Use read_image to view it.]\n");
    }
    Ok(rendered)
}

/// Recoverably bounds one display copy (the dsh `bound`): the retention
/// gate declines non-text/image content silently; every other failure
/// surfaces as the warn reason and keeps the inline content.
///
/// # Errors
///
/// The warn reason (the dsh catch-all template renders it as
/// `spill-policy: <reason>; keeping the inline content`).
pub fn bound(
    seams: &dyn SpillSeams,
    max_tokens: u64,
    session_id: Option<&str>,
    content: &[fr_wire::ContentBlock],
    tool_name: &str,
    call_id: &str,
    label: &str,
) -> Result<Option<Vec<fr_wire::ContentBlock>>, String> {
    if !retainable(content) {
        return Ok(None);
    }
    let inner = move || -> Result<Option<Vec<fr_wire::ContentBlock>>, String> {
        let images: Vec<fr_wire::ContentBlock> = content
            .iter()
            .filter(|block| is_image(block))
            .cloned()
            .collect();

        let route = if images.is_empty() {
            None
        } else {
            let owner = session_id.ok_or_else(|| no_session_owner(tool_name, label))?;
            resolve_route(seams, owner)?
        };
        let price = build_price(seams, &images, route.as_ref())?;
        let total: u64 = content.iter().map(&price).sum();
        if total <= max_tokens {
            return Ok(None);
        }
        let owner = session_id.ok_or_else(|| no_session_owner(tool_name, label))?;
        let full = full_text(seams, content)?;
        let reference = seams.save_text(&crate::spill::SaveTextSpill {
            owner: crate::spill::SpillOwner {
                session_id: owner.to_owned(),
            },
            source: crate::spill::SpillSource::Tool {
                tool_name: tool_name.to_owned(),
                call_id: call_id.to_owned(),
                label: label.to_owned(),
            },
            suggested_name: format!("{tool_name}.txt"),
            content: full,
        })?;
        let total_bytes = total_text_bytes(content);
        let worst_notice = format_spill_notice(
            total_bytes,
            &reference.locator,
            &reference.retrieval_hint,
            images.len() as u64,
        );
        let reserved = price(&text_block(GAP_TEXT))
            + price(&text_block(&format!("{FOOTER_SEPARATOR}{worst_notice}")));
        if price(&text_block(&worst_notice)) > max_tokens {
            return Err(notice_exceeds_cap(tool_name));
        }
        let retained = retain_content(content, max_tokens.saturating_sub(reserved), &price);
        let footer = format_spill_notice(
            retained.omitted_bytes,
            &reference.locator,
            &reference.retrieval_hint,
            retained.omitted_images,
        );
        let result: Vec<fr_wire::ContentBlock> =
            if retained.head.is_empty() && retained.tail.is_empty() {
                vec![text_block(&footer)]
            } else {
                let mut blocks = retained.head;
                blocks.push(text_block(GAP_TEXT));
                blocks.extend(retained.tail);
                blocks.push(text_block(&format!("{FOOTER_SEPARATOR}{footer}")));
                blocks
            };
        Ok(Some(merge_adjacent_text(result)))
    };
    inner()
}

/// The owner-missing refusal (the dsh verbatim template).
fn no_session_owner(tool_name: &str, label: &str) -> String {
    format!("no session owner for {tool_name} {label}")
}

/// The worst-notice-over-budget refusal (the dsh verbatim text; the fr
/// config key rides the kebab conversion).
fn notice_exceeds_cap(tool_name: &str) -> String {
    format!("spill notice for {tool_name} exceeds max-inline-tokens")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::SaveTextSpill;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// The scripted seams double: explicit estimate entries, a scripted
    /// price list, one access path, two route arms, and recorded saves.
    struct FakeSeams {
        estimates: Mutex<HashMap<String, u64>>,
        prices: Mutex<Vec<ImagePrice>>,
        access: Mutex<Option<String>>,
        header_route: Option<(String, String)>,
        agent_route: Option<(String, String)>,
        saves: Mutex<Vec<SaveTextSpill>>,
        locator: String,
        retrieval_hint: String,
    }

    impl FakeSeams {
        fn new() -> Self {
            Self {
                estimates: Mutex::new(HashMap::new()),
                prices: Mutex::new(Vec::new()),
                access: Mutex::new(None),
                header_route: None,
                agent_route: None,
                saves: Mutex::new(Vec::new()),
                locator: "/spill/huge.txt".to_owned(),
                retrieval_hint:
                    "Use read with offset/limit, or grep this path to search within it.".to_owned(),
            }
        }

        fn script_estimate(&self, text: &str, tokens: u64) {
            self.estimates
                .lock()
                .expect("lock")
                .insert(text.to_owned(), tokens);
        }

        fn saves(&self) -> Vec<SaveTextSpill> {
            self.saves.lock().expect("lock").clone()
        }
    }

    impl SpillSeams for FakeSeams {
        fn estimate(&self, blocks: &[fr_wire::ContentBlock]) -> Result<u64, String> {
            let text = block_text(
                blocks
                    .first()
                    .ok_or_else(|| "estimate requires one block".to_owned())?,
            )
            .ok_or_else(|| "estimate prices text only".to_owned())?;
            Ok(*self
                .estimates
                .lock()
                .expect("lock")
                .entry(text.to_owned())
                .or_insert_with(|| (text.chars().count() as u64).div_ceil(4) + 4))
        }

        fn price_images(
            &self,
            _provider: &str,
            _model: &str,
            images: &[fr_wire::ContentBlock],
        ) -> Result<Vec<ImagePrice>, String> {
            let prices = self.prices.lock().expect("lock").clone();
            debug_assert!(prices.is_empty() || prices.len() == images.len());
            Ok(prices)
        }

        fn image_access(&self, _reference: &fr_wire::ImageAttachmentRef) -> Option<String> {
            self.access.lock().expect("lock").clone()
        }

        fn request_header_route(
            &self,
            _session_id: &str,
        ) -> Result<Option<(String, String)>, String> {
            Ok(self.header_route.clone())
        }

        fn agent_route(&self, _session_id: &str) -> Result<Option<(String, String)>, String> {
            Ok(self.agent_route.clone())
        }

        fn save_text(&self, save: &SaveTextSpill) -> Result<crate::spill::SpillRef, String> {
            self.saves.lock().expect("lock").push(save.clone());
            Ok(crate::spill::SpillRef {
                locator: self.locator.clone(),
                bytes: save.content.len() as u64,
                retrieval_hint: self.retrieval_hint.clone(),
            })
        }
    }

    fn text_of(block: &fr_wire::ContentBlock) -> &str {
        block_text(block).unwrap_or_default()
    }

    /// `spill-policy` unit: the surrogate-safe slice keeps pairs whole on
    /// both ends (the dsh `textSlice` fixup).
    #[test]
    fn text_slice_keeps_surrogate_pairs_whole() {
        // "a" + U+1F600 (2 code units) + "b": total 4 units.
        let text = "a\u{1F600}b";
        assert_eq!(text.encode_utf16().count(), 4);
        // The head cut at 2 lands between the pair: the pair drops.
        assert_eq!(text_slice(text, 2, false), "a");
        // The head cut at 1 is clean.
        assert_eq!(text_slice(text, 1, false), "a");
        // The tail cut at 2 units lands between the pair and restores
        // the whole-pair boundary from the pair end (the dsh fixup: a
        // lone high surrogate is never introduced).
        assert_eq!(text_slice(text, 2, true), "b");
        assert_eq!(text_slice(text, 1, true), "b");
        // A cut before the pair keeps the pair in the tail.
        assert_eq!(text_slice(text, 3, true), "\u{1F600}b");
        assert_eq!(text_slice(text, 4, false), text);
        assert_eq!(text_slice(text, 4, true), text);
        assert_eq!(text_slice(text, 0, false), "");
        assert_eq!(text_slice(text, 0, true), "");
    }

    /// `spill-policy` unit: the binary search fits the largest end under
    /// a monotonic estimate, including the exact-fit boundary.
    #[test]
    fn fit_text_finds_the_largest_fitting_end() {
        let text = "abcdefgh"; // 8 chars: ceil(n/4)+4 tokens under the default rule.
        let seams = FakeSeams::new();
        let price = |block: &fr_wire::ContentBlock| -> u64 {
            seams
                .estimate(std::slice::from_ref(block))
                .unwrap_or_default()
        };
        // Budget 8 fits "abcdefgh" (ceil(8/4)+4 = 6 <= 8); budget 5 fits
        // "abcd" head and "efgh" tail ("defgh" prices 6 > 5).
        assert_eq!(fit_text(text, 6, false, &price), text);
        assert_eq!(fit_text(text, 5, false, &price), "abcd");
        assert_eq!(fit_text(text, 5, true, &price), "efgh");
        assert_eq!(fit_text(text, 0, false, &price), "");
    }

    /// `spill-policy` unit: the retention keeps whole blocks at the ends,
    /// leaves unused space at an indivisible image, and splits the one
    /// shared text block at the fitted head length.
    #[test]
    fn retain_content_splits_ends_and_keeps_images_whole() {
        let seams = FakeSeams::new();
        let price = |block: &fr_wire::ContentBlock| -> u64 {
            seams
                .estimate(std::slice::from_ref(block))
                .unwrap_or_default()
        };
        // "aaaa...(40 chars)" prices 14; budget 12: head fits 8 chars
        // (6), tail fits 8 (6); the middle 24 are omitted.
        let head40 = "a".repeat(40);
        let content = vec![text_block(&head40)];
        let retained = retain_content(&content, 12, &price);
        assert_eq!(retained.head.len(), 1);
        assert_eq!(text_of(&retained.head[0]).chars().count(), 8);
        assert_eq!(retained.tail.len(), 1);
        assert_eq!(text_of(&retained.tail[0]).chars().count(), 8);
        assert_eq!(retained.omitted_bytes, 24);
        assert_eq!(retained.omitted_images, 0);

        // An over-budget image between two small texts: the image is
        // indivisible, so it is omitted whole and the budget stays
        // unused around it.
        let image: fr_wire::ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "image",
            "attachment": {"attachmentId": "sha256:x", "mediaType": "image/png", "bytes": 8, "width": 2, "height": 2},
        }))
        .expect("image block");
        // The image prices 100 via its scripted visual cost.
        let image_costs = |block: &fr_wire::ContentBlock| -> u64 {
            if is_image(block) { 100 } else { price(block) }
        };
        let content = vec![text_block("abcd"), image, text_block("efgh")];
        let retained = retain_content(&content, 100, &image_costs);
        // Head: "abcd" (5) fits; the image (100) does not (budget split
        // gives head 50) and stays out; tail: "efgh" (5) fits.
        assert_eq!(retained.head.len(), 1);
        assert_eq!(retained.tail.len(), 1);
        assert_eq!(retained.omitted_images, 1);
        assert_eq!(retained.omitted_bytes, 0);

        // A single text block: the head fit and the tail resume after
        // the head's cut (the dsh shared-block split).
        let content = vec![text_block(&"x".repeat(40))];
        let retained = retain_content(&content, 12, &price);
        assert_eq!(
            text_of(&retained.head[0]).chars().count() + text_of(&retained.tail[0]).chars().count(),
            16
        );
        assert_eq!(retained.omitted_bytes, 24);
    }

    /// `spill-policy` unit: the notice renders its persisted spelling
    /// with and without the image clause.
    #[test]
    fn notice_renders_the_persisted_spelling() {
        let hint = "Use read with offset/limit, or grep this path to search within it.";
        assert_eq!(
            format_spill_notice(1_234, "/spill/x.txt", hint, 0),
            "(Omitted 1234 bytes. Full formatted result stored at: /spill/x.txt. \
             Use read with offset/limit, or grep this path to search within it.)"
        );
        assert_eq!(
            format_spill_notice(7, "/spill/y.txt", hint, 2),
            "(Omitted 7 bytes. Omitted 2 images. Full formatted result stored at: \
             /spill/y.txt. Use read with offset/limit, or grep this path to search within it.)"
        );
    }

    /// `spill-policy` unit: adjacent text blocks merge into one framing
    /// cost; non-text blocks stay in place.
    #[test]
    fn adjacent_text_merges() {
        let merged = merge_adjacent_text(vec![
            text_block("a"),
            text_block("b"),
            serde_json::from_value(serde_json::json!({
                "type": "image",
                "attachment": {"attachmentId": "sha256:x", "mediaType": "image/png", "bytes": 8, "width": 2, "height": 2},
            }))
            .expect("image"),
            text_block("c"),
        ]);
        assert_eq!(merged.len(), 3);
        assert_eq!(text_of(&merged[0]), "ab");
        assert!(is_image(&merged[1]));
        assert_eq!(text_of(&merged[2]), "c");
    }

    /// The default bound fixture: a 40-kilobyte plain-text result that
    /// the 12-token cap must spill.
    fn over_cap_text() -> Vec<fr_wire::ContentBlock> {
        vec![text_block(&"a".repeat(400))]
    }

    fn seeded_seams() -> FakeSeams {
        let seams = FakeSeams::new();
        seams.script_estimate("a".repeat(400).as_str(), 104);
        seams
    }

    /// `bound`: a within-cap result stays inline and saves nothing.
    #[test]
    fn bound_within_cap_keeps_the_inline_content() {
        let seams = FakeSeams::new();
        let retained = bound(
            &seams,
            12_500,
            Some("session-1"),
            &[text_block("tiny")],
            "huge_read",
            "call-1",
            "result",
        )
        .expect("bound succeeds");
        assert!(retained.is_none());
        assert_eq!(seams.saves().as_slice(), [], "nothing saved");
    }

    /// `bound`: an over-cap plain-text result spills the complete text
    /// and composes head + gap + tail + the verbatim footer.
    #[test]
    fn bound_spills_the_full_text_and_composes_the_replacement() {
        let seams = seeded_seams();
        let content = over_cap_text();
        let retained = bound(
            &seams,
            70,
            Some("session-1"),
            &content,
            "huge_read",
            "call-1",
            "result",
        )
        .expect("bound succeeds")
        .expect("over the cap spills");
        // The full text rides the save with the dsh source shape.
        let saves = seams.saves();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].owner.session_id, "session-1");
        assert_eq!(saves[0].suggested_name, "huge_read.txt");
        assert_eq!(
            saves[0].source,
            crate::spill::SpillSource::Tool {
                tool_name: "huge_read".to_owned(),
                call_id: "call-1".to_owned(),
                label: "result".to_owned(),
            }
        );
        assert_eq!(saves[0].content.len(), 400);
        // The inline copy merges into one text block (the dsh final
        // pass): head, gap, tail, and the footer all carry text.
        assert_eq!(retained.len(), 1);
        let inline = text_of(&retained[0]);
        assert!(
            inline.starts_with("aa") && inline.contains(GAP_TEXT),
            "the head and the gap open the inline copy: {inline}"
        );
        let footer = inline
            .rfind("(Omitted ")
            .map(|at| &inline[at..])
            .expect("the footer rides the end");
        assert!(
            footer.contains(" bytes. Full formatted result stored at: /spill/huge.txt. ")
                && footer.ends_with(
                    "Use read with offset/limit, or grep this path to search within it.)"
                ),
            "verbatim footer shape: {footer}"
        );
        // The footer counts exactly the omitted bytes (400 - the kept
        // head and tail).
        let head = inline.find(GAP_TEXT).expect("the gap splits the kept ends");
        let after_gap = &inline[head + GAP_TEXT.len()..];
        let tail = after_gap
            .find("\n\n(Omitted")
            .expect("the footer follows the tail");
        let omitted = 400 - head - tail;
        assert_eq!((head, tail), (32, 32), "the budget halves hold 32 each");
        assert!(
            footer.contains(&format!("Omitted {omitted} bytes.")),
            "footer counts the omitted bytes (kept={omitted}): {footer}"
        );
    }

    /// `bound`: non-text content declines silently (the dsh
    /// `retainable` gate).
    #[test]
    fn bound_declines_non_text_content_silently() {
        let seams = FakeSeams::new();
        let reasoning: fr_wire::ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "reasoning",
            "text": "why",
        }))
        .expect("reasoning block");
        let retained = bound(
            &seams,
            4,
            Some("session-1"),
            &[reasoning],
            "mixed",
            "call-1",
            "result",
        )
        .expect("bound succeeds");
        assert!(retained.is_none());
        assert_eq!(seams.saves().as_slice(), [], "nothing saved");
    }

    /// `bound`: an image occurrence without a route keeps the inline
    /// content with the dsh no-calculator refusal.
    #[test]
    fn bound_images_without_a_route_keep_the_inline_content() {
        let seams = FakeSeams::new();
        let image: fr_wire::ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "image",
            "attachment": {"attachmentId": "sha256:x", "mediaType": "image/png", "bytes": 8, "width": 2, "height": 2},
        }))
        .expect("image");
        let error = bound(
            &seams,
            4,
            Some("session-1"),
            &[image],
            "shot",
            "call-1",
            "result",
        )
        .expect_err("no route refuses");
        assert_eq!(error, "the current model has no image token calculator");
        assert_eq!(seams.saves().as_slice(), [], "nothing saved");
    }

    /// `bound`: an image occurrence prices through the route (visual
    /// tokens plus the descriptor text) and spills the rendered image
    /// line with the execution-world path.
    #[test]
    fn bound_images_price_and_render_the_access_path() {
        let mut seams = FakeSeams::new();
        seams.header_route = Some(("deepseek".to_owned(), "v3".to_owned()));
        *seams.access.lock().expect("lock") = Some("/exec/shot.png".to_owned());
        *seams.prices.lock().expect("lock") = vec![ImagePrice {
            visual_tokens: 900,
            text: "image handle text".to_owned(),
        }];
        seams.script_estimate("image handle text", 5);
        let image: fr_wire::ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "image",
            "attachment": {"attachmentId": "sha256:shot", "mediaType": "image/png", "bytes": 8, "width": 4, "height": 3},
        }))
        .expect("image");
        let retained = bound(
            &seams,
            70,
            Some("session-1"),
            &[image],
            "shot",
            "call-1",
            "result",
        )
        .expect("bound succeeds")
        .expect("over the cap spills");
        let saved = &seams.saves()[0];
        assert_eq!(
            saved.content,
            "\n[Image: \"/exec/shot.png\"; \"image/png\"; 4x3. Use read_image to view it.]\n"
        );
        assert_eq!(retained.len(), 1);
        let footer = text_of(&retained[0]);
        assert!(
            footer.contains("Omitted 1 images."),
            "the whole image is omitted: {footer}"
        );
    }

    /// `bound`: an unreadable attachment path keeps the inline content
    /// (the dsh refusal text interpolates the attachment id).
    #[test]
    fn bound_unreadable_path_keeps_the_inline_content() {
        let mut seams = FakeSeams::new();
        seams.header_route = Some(("deepseek".to_owned(), "v3".to_owned()));
        *seams.prices.lock().expect("lock") = vec![ImagePrice {
            visual_tokens: 900,
            text: "t".to_owned(),
        }];
        // No access path scripted: the absent arm.
        let image: fr_wire::ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "image",
            "attachment": {"attachmentId": "sha256:shot", "mediaType": "image/png", "bytes": 8, "width": 4, "height": 3},
        }))
        .expect("image");
        let error = bound(
            &seams,
            12,
            Some("session-1"),
            &[image],
            "shot",
            "call-1",
            "result",
        )
        .expect_err("unreadable path refuses");
        assert_eq!(error, "image sha256:shot has no readable attachment path");
    }

    /// `bound`: an owner-less call refuses with the dsh template.
    #[test]
    fn bound_owner_less_call_refuses() {
        let seams = FakeSeams::new();
        let error = bound(
            &seams,
            4,
            None,
            over_cap_text().as_slice(),
            "huge_read",
            "call-1",
            "result",
        )
        .expect_err("no owner refuses");
        assert_eq!(error, "no session owner for huge_read result");
    }

    /// `bound`: a notice that alone exceeds the cap refuses with the
    /// dsh template (the fr key rides the kebab conversion).
    #[test]
    fn bound_notice_over_cap_refuses() {
        let seams = seeded_seams();
        // A cap the content exceeds but the notice exceeds too.
        let error = bound(
            &seams,
            6,
            Some("session-1"),
            over_cap_text().as_slice(),
            "huge_read",
            "call-1",
            "result",
        )
        .expect_err("notice over cap refuses");
        assert_eq!(
            error,
            "spill notice for huge_read exceeds max-inline-tokens"
        );
    }
}
