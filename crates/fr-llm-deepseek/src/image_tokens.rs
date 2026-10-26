//! `DeepSeek` vision-token accounting (dsh `image-tokens.ts`): the
//! provider's published image-token calculator ported verbatim in its
//! current `v41` configuration.
//!
//! The provider scales an image below
//! 544x544 total pixels up, aligns it to a 14px-patch grid, downsamples
//! 3:1 per axis into token cells, and caps one image at 1024 tokens by
//! solving the largest aspect-preserving grid inside that budget. The
//! count is exact: this configuration has no alignment pad and no
//! aspect-ratio clamp. Actual usage remains authoritative.

use fr_llm::projection::{ProjectedDimensions, long_edge_dimensions};

/// Vision patch edge in pixels (dsh `PATCH_SIZE`).
const PATCH_SIZE: f64 = 14.0;
/// Per-axis patch-to-token downsampling ratio (dsh `DOWNSAMPLE_RATIO`).
const DOWNSAMPLE_RATIO: f64 = 3.0;
/// Provider cap on tokens for one request image (dsh `MAX_IMAGE_TOKENS`).
const MAX_IMAGE_TOKENS: f64 = 1024.0;
/// Total-pixel floor; smaller images are scaled up before grid projection
/// (dsh `MIN_PIXELS`).
const MIN_PIXELS: f64 = 544.0 * 544.0;
/// Pixels covered by one token cell along either axis (dsh `CELL_SIZE`).
const CELL_SIZE: f64 = PATCH_SIZE * DOWNSAMPLE_RATIO;

fn int_div(value: f64, divisor: f64) -> f64 {
    (value / divisor).floor()
}

fn ceil_div(value: f64, divisor: f64) -> f64 {
    ((value + divisor - 1.0) / divisor).floor()
}

/// One scale-pad-project pass result (dsh `GridResize`); every member is
/// an integral float exactly as the JS arithmetic produces.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GridResize {
    grid_height: f64,
    grid_width: f64,
    best_height: f64,
    best_width: f64,
    num_tokens: f64,
}

/// Token count of one grid: every row carries a separator, plus two
/// framing tokens (dsh `gridTokens`).
#[expect(
    clippy::suboptimal_flops,
    reason = "JS Number parity: dsh computes h*(w+1)+2 with separate IEEE 754 roundings"
)]
fn grid_tokens(grid_height: f64, grid_width: f64) -> f64 {
    grid_height * (grid_width + 1.0) + 2.0
}

/// Token-cell count along one padded pixel axis (dsh `gridCells`).
fn grid_cells(padded_length: f64) -> f64 {
    ceil_div(int_div(padded_length, PATCH_SIZE), DOWNSAMPLE_RATIO)
}

/// Solve the largest grid within `budget` tokens preserving the aspect
/// ratio (dsh `solveResizeRatio`).
fn solve_resize_ratio(height: f64, width: f64, budget: f64) -> GridResize {
    let aspect = height / width;
    let ideal_grid_width = ((budget - 2.0) / aspect + 0.25).sqrt() - 0.5;
    let ideal_grid_height = ideal_grid_width * aspect;
    let (best_width, best_height);
    if ideal_grid_width < 1.0 {
        let solved_grid_width = 1.0;
        let solved_grid_height = int_div(budget - 2.0, solved_grid_width + 1.0);
        best_width = solved_grid_width * CELL_SIZE;
        best_height = solved_grid_height * CELL_SIZE;
    } else if ideal_grid_height < 1.0 {
        let solved_grid_height = 1.0;
        let solved_grid_width = int_div(budget - 2.0, solved_grid_height) - 1.0;
        best_width = solved_grid_width * CELL_SIZE;
        best_height = solved_grid_height * CELL_SIZE;
    } else {
        let solved_grid_width = ideal_grid_width.trunc();
        let solved_grid_height = ideal_grid_height.trunc();
        let scale = f64::min(
            solved_grid_width * CELL_SIZE / width,
            solved_grid_height * CELL_SIZE / height,
        );
        best_width = (width * scale / PATCH_SIZE).trunc() * PATCH_SIZE;
        best_height = (height * scale / PATCH_SIZE).trunc() * PATCH_SIZE;
    }
    let grid_height = grid_cells(best_height);
    let grid_width = grid_cells(best_width);
    GridResize {
        grid_height,
        grid_width,
        best_height,
        best_width,
        num_tokens: grid_tokens(grid_height, grid_width),
    }
}

/// Project padded pixel dimensions onto the largest in-budget token grid
/// (dsh `safeResize`).
fn safe_resize(height: f64, width: f64, padded_height: f64, padded_width: f64) -> GridResize {
    let grid_height = grid_cells(padded_height);
    let grid_width = grid_cells(padded_width);
    let direct = GridResize {
        grid_height,
        grid_width,
        best_height: padded_height,
        best_width: padded_width,
        num_tokens: grid_tokens(grid_height, grid_width),
    };
    if direct.num_tokens <= MAX_IMAGE_TOKENS {
        return direct;
    }
    let solved = solve_resize_ratio(height, width, MAX_IMAGE_TOKENS);
    if solved.num_tokens > MAX_IMAGE_TOKENS {
        unreachable!("deepseek image tokens: no grid fits the token budget for {width}x{height}") // unreachable: the closed-form solve stays within the budget for every positive geometry
    }
    solved
}

/// One scale-pad-project pass; the caller iterates it to a fixpoint (dsh
/// `resizeOnce`).
fn resize_once(width: f64, height: f64) -> GridResize {
    let mut scaled_width = width;
    let mut scaled_height = height;
    let pixels = scaled_width * scaled_height;
    if pixels < MIN_PIXELS && pixels > 0.0 {
        let scale = (MIN_PIXELS / pixels).sqrt();
        scaled_width = (scaled_width * scale).trunc();
        scaled_height = (scaled_height * scale).trunc();
    }
    let padded_width = ceil_div(scaled_width, PATCH_SIZE) * PATCH_SIZE;
    let padded_height = ceil_div(scaled_height, PATCH_SIZE) * PATCH_SIZE;
    safe_resize(scaled_height, scaled_width, padded_height, padded_width)
}

#[expect(
    clippy::float_cmp,
    reason = "JS Number parity: grid members are integral f64 values compared for the exact fixpoint (dsh deep equality)"
)]
fn same_resize(left: GridResize, right: GridResize) -> bool {
    left.grid_height == right.grid_height
        && left.grid_width == right.grid_width
        && left.best_height == right.best_height
        && left.best_width == right.best_width
        && left.num_tokens == right.num_tokens
}

/// Render one integral token count (every grid value is a small
/// non-negative integer computed exactly in f64).
const fn token_count(value: f64) -> u64 {
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "grid token counts are small non-negative integers exact in f64"
    )]
    {
        value as u64
    }
}

/// Render one integral grid dimension (patch-aligned lengths are small
/// positive integers exact in f64).
const fn token_dimension(value: f64) -> u32 {
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "patch-aligned dimensions are small positive integers exact in f64"
    )]
    {
        value as u32
    }
}

/// Vision tokens `DeepSeek` charges for one request image of the given
/// dimensions (dsh `deepSeekImageTokens`): at most 1024.
///
/// # Panics
/// Only if the published solver's fixpoint iteration failed to converge
/// within 10 passes — unreachable because every pass is a projection, so
/// a second identical pass is a fixpoint.
#[must_use]
pub fn deep_seek_image_tokens(width: u32, height: u32) -> u64 {
    let mut result = resize_once(f64::from(width), f64::from(height));
    for _iteration in 1..10 {
        let next = resize_once(result.best_width, result.best_height);
        if same_resize(next, result) {
            return token_count(result.num_tokens);
        }
        result = next;
    }
    // Every pass is a projection, so a second identical pass is a fixpoint:
    // the loop provably converges and this line is unreachable defense
    // mirroring the published solver's guard.
    panic!("deepseek image tokens: resize did not converge for {width}x{height}"); // invariant: unreachable convergence guard of the published solver
}

/// Dimensions the harness sends so the provider keeps the whole image (dsh
/// `deepSeekRequestImageDimensions`).
///
/// The source itself when its
/// patch-padded grid fits the token cap, otherwise the source aspect ratio
/// at the solved grid's long edge.
///
/// The provider pads the short edge to
/// whole patches on its side. Small images are never enlarged.
#[must_use]
pub fn deep_seek_request_image_dimensions(width: u32, height: u32) -> ProjectedDimensions {
    let padded_width = ceil_div(f64::from(width), PATCH_SIZE) * PATCH_SIZE;
    let padded_height = ceil_div(f64::from(height), PATCH_SIZE) * PATCH_SIZE;
    if grid_tokens(grid_cells(padded_height), grid_cells(padded_width)) <= MAX_IMAGE_TOKENS {
        return ProjectedDimensions { width, height };
    }
    let solved = solve_resize_ratio(f64::from(height), f64::from(width), MAX_IMAGE_TOKENS);
    if width >= height {
        long_edge_dimensions(width, height, token_dimension(solved.best_width))
    } else {
        long_edge_dimensions(width, height, token_dimension(solved.best_height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v41 calculator pins the published values across the small-image
    /// upscale, the patch grid, and the solved-cap arms (values replicated
    /// independently from the TS source arithmetic).
    #[test]
    fn image_tokens_pin_v41_calculator() {
        // 544x544 sits exactly at the pixel floor: 13x13 cells.
        assert_eq!(deep_seek_image_tokens(544, 544), 184);
        // A tiny image is upscaled to the pixel floor first.
        assert_eq!(deep_seek_image_tokens(100, 100), 184);
        // 4096x4096 solves to the largest in-budget grid.
        assert_eq!(deep_seek_image_tokens(4096, 4096), 994);
        assert_eq!(deep_seek_image_tokens(1302, 1302), 994);
        // Landscape and portrait routes.
        assert_eq!(deep_seek_image_tokens(800, 600), 317);
        assert_eq!(deep_seek_image_tokens(1024, 768), 496);
    }

    /// Sent dimensions keep the source when its padded grid fits and
    /// otherwise follow the solved long edge; small images are never
    /// enlarged.
    #[test]
    fn request_dimensions_pin_source_or_solved_edge() {
        // The padded grid of 500x300 fits: sent as-is.
        assert_eq!(
            deep_seek_request_image_dimensions(500, 300),
            ProjectedDimensions {
                width: 500,
                height: 300
            }
        );
        // 4096x4096 overshoots: solved long edge 1302 on both axes.
        assert_eq!(
            deep_seek_request_image_dimensions(4096, 4096),
            ProjectedDimensions {
                width: 1302,
                height: 1302
            }
        );
        // Landscape and portrait overshoots take their own long edges.
        assert_eq!(
            deep_seek_request_image_dimensions(8000, 6000),
            ProjectedDimensions {
                width: 1512,
                height: 1134
            }
        );
        assert_eq!(
            deep_seek_request_image_dimensions(6000, 8000),
            ProjectedDimensions {
                width: 1134,
                height: 1512
            }
        );
    }
}

// Rust guideline compliant 2026-10-26
