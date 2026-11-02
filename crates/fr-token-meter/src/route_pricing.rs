//! Request-projected surface pricing (dsh `token-meter/route-pricing.ts`
//! 1:1): replaces attachment-block heuristics with the image and file
//! representations sent to the routed model.
//!
//! The dsh pricing object cannot cross the wire, so the meter resolves
//! the route's per-occurrence prices over the `llm` face first and hands
//! the aligned slice here; the occurrence count check stays in this
//! module so a misaligned answer still fails loud.

#![forbid(unsafe_code)]

use fr_llm::adapter::ImageRequestPrice;
use fr_wire::content::FileAttachmentRef;

use crate::estimate::estimate_text_block;
use crate::surface_fold::MeterSurfaceNode;
use crate::types::TokenSurfaceNode;

/// The request-time file-text resolver face (the dsh `fileText`
/// closure shape).
pub type FileTextResolver<'a> = dyn Fn(&FileAttachmentRef) -> Result<String, String> + 'a;

/// The route image-price resolver face (the dsh
/// `LlmImageRequestPricing.priceImages` shape over one exact route).
pub type ImagePriceResolver<'a> =
    dyn Fn(&str, &str, &[fr_wire::content::ContentBlock]) -> Option<Vec<ImageRequestPrice>> + 'a;

/// One surface priced for a request route: public nodes plus their total
/// (dsh `PricedSurface`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricedSurface {
    /// Positional nodes carrying both the route price and the
    /// fixed-heuristic price.
    pub nodes: Vec<TokenSurfaceNode>,
    /// Sum of the route prices across the surface.
    pub surface_tokens: u64,
}

/// The flat image occurrence list across the surface, in node order
/// then message order (the dsh `nodes.flatMap(node => node.images)`).
#[must_use]
pub fn collect_images(nodes: &[MeterSurfaceNode]) -> Vec<fr_wire::content::ContentBlock> {
    nodes
        .iter()
        .flat_map(|node| node.images.iter().cloned())
        .collect()
}

/// Price one ordered surface under its model-request attachment
/// projection (dsh `priceSurface`).
///
/// `pricing` carries the per-occurrence prices for [`collect_images`]
/// over the nodes in order, when the route declares pricing;
/// `file_text` resolves the exact request-time text of a file
/// occurrence when the mounted LLM service projects files.
///
/// # Errors
///
/// Returns the dsh verbatim text when the pricing answers a different
/// occurrence count than it was asked: misalignment would silently
/// misprice nodes, so it must fail loud.
pub fn price_surface(
    nodes: &[MeterSurfaceNode],
    pricing: Option<&[ImageRequestPrice]>,
    file_text: Option<&FileTextResolver>,
) -> Result<PricedSurface, String> {
    let images = collect_images(nodes);
    let has_files = file_text.is_some() && nodes.iter().any(|node| !node.files.is_empty());
    if (pricing.is_none() || images.is_empty()) && !has_files {
        let mut surface_tokens = 0u64;
        let public_nodes = nodes
            .iter()
            .map(|node| {
                surface_tokens = surface_tokens.saturating_add(node.heuristic_tokens);
                TokenSurfaceNode {
                    seq: node.seq,
                    tokens: node.heuristic_tokens,
                    heuristic_tokens: node.heuristic_tokens,
                }
            })
            .collect();
        return Ok(PricedSurface {
            nodes: public_nodes,
            surface_tokens,
        });
    }
    let prices = pricing.unwrap_or(&[]);
    if pricing.is_some() && prices.len() != images.len() {
        return Err(format!(
            "token meter: route image pricing answered {} prices for {} occurrences",
            prices.len(),
            images.len()
        ));
    }
    let mut cursor = 0;
    let mut surface_tokens = 0u64;
    let mut public_nodes = Vec::with_capacity(nodes.len());
    for node in nodes {
        let mut tokens = node.heuristic_tokens;
        if let Some(file_text) = file_text
            && !node.files.is_empty()
        {
            tokens = tokens.saturating_sub(node.file_structural_tokens);
            for file in &node.files {
                let text = file_text(file)?;
                tokens = tokens.saturating_add(estimate_text_block(&text));
            }
        }
        if pricing.is_some() && !node.images.is_empty() {
            tokens = tokens.saturating_sub(node.image_structural_tokens);
            for _ in 0..node.images.len() {
                let price = &prices[cursor];
                cursor += 1;
                tokens = tokens.saturating_add(
                    price
                        .visual_tokens
                        .saturating_add(estimate_text_block(&price.text)),
                );
            }
        }
        surface_tokens = surface_tokens.saturating_add(tokens);
        public_nodes.push(TokenSurfaceNode {
            seq: node.seq,
            tokens,
            heuristic_tokens: node.heuristic_tokens,
        });
    }
    Ok(PricedSurface {
        nodes: public_nodes,
        surface_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimate::ROLE_OVERHEAD;
    use fr_wire::content::KnownContentBlock;

    fn image_block(id: &str, offloaded: bool) -> fr_wire::content::ContentBlock {
        fr_wire::content::ContentBlock::Known(KnownContentBlock::Image {
            attachment: fr_wire::content::ImageAttachmentRef {
                attachment_id: fr_wire::ids::AttachmentId::new(id),
                media_type: fr_wire::content::ImageMediaType::Png,
                bytes: 64,
                width: 8,
                height: 8,
                name: None,
                original_dimensions: None,
            },
            offloaded: offloaded.then_some(fr_wire::content::TrueOnly),
        })
    }

    fn file_block(id: &str) -> fr_wire::content::ContentBlock {
        fr_wire::content::ContentBlock::Known(KnownContentBlock::File {
            attachment: FileAttachmentRef {
                attachment_id: fr_wire::ids::AttachmentId::new(id),
                name: "a.txt".to_owned(),
                bytes: 16,
            },
        })
    }

    fn node(
        seq: u64,
        heuristic: u64,
        blocks: &[fr_wire::content::ContentBlock],
    ) -> MeterSurfaceNode {
        let mut images = Vec::new();
        let mut files = Vec::new();
        for block in blocks {
            match block {
                fr_wire::content::ContentBlock::Known(KnownContentBlock::Image { .. }) => {
                    images.push(block.clone());
                }
                fr_wire::content::ContentBlock::Known(KnownContentBlock::File { attachment }) => {
                    files.push(attachment.clone());
                }
                _ => {}
            }
        }
        let image_structural: u64 = images
            .iter()
            .map(crate::estimate::estimate_structural_block)
            .sum();
        let file_structural: u64 = files
            .iter()
            .map(|file| {
                crate::estimate::estimate_structural_block(&fr_wire::content::ContentBlock::Known(
                    KnownContentBlock::File {
                        attachment: file.clone(),
                    },
                ))
            })
            .sum();
        MeterSurfaceNode {
            seq,
            heuristic_tokens: heuristic,
            image_structural_tokens: image_structural,
            file_structural_tokens: file_structural,
            images,
            files,
        }
    }

    /// The image-free, file-free surface prices every node at its
    /// heuristic and totals the sum.
    #[test]
    fn heuristic_passthrough_prices_plain_nodes() {
        let nodes = [node(1, ROLE_OVERHEAD + 20, &[]), node(2, 10, &[])];
        let priced = price_surface(&nodes, None, None).expect("prices");
        assert_eq!(priced.surface_tokens, 30 + ROLE_OVERHEAD);
        assert!(priced.nodes.iter().all(|n| n.tokens == n.heuristic_tokens));
    }

    /// Route pricing replaces each image's structural price with its
    /// visual tokens plus the priced substitute text; the cursor walks
    /// occurrences in node order.
    #[test]
    fn image_prices_replace_structural_and_align_by_cursor() {
        let nodes = [
            node(1, 100, &[image_block("a", false)]),
            node(2, 100, &[image_block("b", false), image_block("c", false)]),
        ];
        let images = collect_images(&nodes);
        assert_eq!(images.len(), 3);
        let prices: Vec<ImageRequestPrice> = images
            .iter()
            .map(|_| ImageRequestPrice {
                visual_tokens: 7,
                text: "handle".to_owned(),
            })
            .collect();
        let surface = price_surface(&nodes, Some(&prices), None).expect("prices");
        let text_price = crate::estimate::estimate_text_block("handle");
        let structural = crate::estimate::estimate_structural_block(&image_block("a", false));
        for entry in &surface.nodes {
            let per_image = 7u64.saturating_add(text_price);
            let images_on_node = u64::from(if entry.seq == 1 { 1u8 } else { 2u8 });
            let expected_tokens = 100u64
                .saturating_sub(structural.saturating_mul(images_on_node))
                .saturating_add(per_image.saturating_mul(images_on_node));
            assert_eq!(entry.tokens, expected_tokens, "node {}", entry.seq);
        }
        assert_eq!(
            surface.surface_tokens,
            surface.nodes.iter().map(|n| n.tokens).sum::<u64>()
        );
    }

    /// A misaligned pricing answer fails loud with the dsh verbatim
    /// text.
    #[test]
    fn misaligned_prices_fail_loud() {
        let nodes = [node(1, 100, &[image_block("a", false)])];
        let prices = vec![];
        let error = price_surface(&nodes, Some(&prices), None).expect_err("misaligned");
        assert_eq!(
            error,
            "token meter: route image pricing answered 0 prices for 1 occurrences"
        );
    }

    /// File projection replaces the file structural price with the
    /// priced handle text.
    #[test]
    fn file_text_replaces_file_structural_price() {
        let nodes = [node(1, 100, &[file_block("f")])];
        let file_text =
            |file: &FileAttachmentRef| Ok::<String, String>(format!("file {}", file.name));
        let priced = price_surface(&nodes, None, Some(&file_text)).expect("prices");
        let expected = 100u64
            .saturating_sub(crate::estimate::estimate_structural_block(&file_block("f")))
            .saturating_add(crate::estimate::estimate_text_block("file a.txt"));
        assert_eq!(priced.nodes[0].tokens, expected);
    }
}
