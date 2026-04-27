//! Expands opaque pixel colors into fully-transparent border pixels.
//! This prevents dark-fringe artifacts when images are scaled or filtered,
//! particularly visible at spritesheet sprite boundaries.
//!
//! Algorithm: BFS outward from every opaque pixel, averaging neighbor colors
//! into each transparent pixel it reaches. Alpha stays 0 — only RGB is written.
//!
//! Optimizations over the Tarmac version:
//! - No per-call `adjacent()` Vec allocations — neighbor offsets are computed
//!   directly as flat index deltas using pre-calculated row stride.
//! - Two-queue swap instead of a `processed` Vec per wave — avoids one
//!   allocation per BFS level.
//! - Raw pixel slice access (`img.as_mut()`) instead of `get_pixel` /
//!   `put_pixel` to skip per-call bounds checks inside the hot loop.
//! - Single init pass: mark opaque pixels, then seed border-transparent pixels
//!   in the same scan without calling `adjacent()`.

use bit_vec::BitVec;
#[allow(unused_imports)]
use image::{Rgba, RgbaImage};
use std::collections::VecDeque;

pub fn alpha_bleed(img: &mut RgbaImage) {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return;
    }

    let len = (w * h) as usize;

    // can_be_sampled: pixel has a stable color that neighbours may average.
    // visited:        pixel is already enqueued or processed — don't enqueue again.
    let mut can_be_sampled = BitVec::from_elem(len, false);
    let mut visited = BitVec::from_elem(len, false);

    // Init pass
    //
    // Mark every opaque pixel as samplable + visited.
    // Simultaneously seed the BFS queue with transparent pixels that border
    // at least one opaque pixel, checking only the 4 cardinal neighbours to
    // keep the scan O(w*h) with no inner allocations.
    let pixels = img.as_raw(); // flat RGBA bytes, row-major
    let mut current: VecDeque<u32> = VecDeque::new();

    for i in 0..len {
        let alpha = pixels[i * 4 + 3];
        if alpha != 0 {
            can_be_sampled.set(i, true);
            visited.set(i, true);
        }
    }

    // Seed: transparent pixels adjacent to any opaque pixel.
    // We check all 8 neighbors here too for correctness, but without Vec allocs.
    for y in 0..h {
        for x in 0..w {
            let i = (x + y * w) as usize;
            if can_be_sampled[i] {
                continue; // already opaque
            }
            // Check 8 neighbours via clamped coordinates.
            let borders_opaque = OFFSETS_8.iter().any(|&(dx, dy)| {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                nx >= 0
                    && ny >= 0
                    && nx < w as i32
                    && ny < h as i32
                    && can_be_sampled[(nx as u32 + ny as u32 * w) as usize]
            });
            if borders_opaque {
                visited.set(i, true);
                current.push_back(i as u32);
            }
        }
    }

    // Wave-front BFS
    //
    // We process level-by-level so that each transparent pixel samples only
    // from pixels that were already stable (can_be_sampled) when its wave
    // began — preventing blended colors from propagating into later waves.
    //
    // Two-queue swap: `current` holds this wave, `next` accumulates the next.
    // After processing `current`, mark everything in it as samplable, then swap.
    let mut next: VecDeque<u32> = VecDeque::new();

    // Safety: we access the raw pixel slice directly to avoid bounds checks in
    // the inner loop. All index arithmetic is guarded by the coord clamp above.
    let pixels = img.as_mut(); // &mut [u8]

    while !current.is_empty() {
        // Process every pixel in the current wave.
        for &flat in &current {
            let i = flat as usize;
            let x = flat % w;
            let y = flat / w;

            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            let mut count = 0u32;

            for &(dx, dy) in OFFSETS_8.iter() {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let ni = (nx as u32 + ny as u32 * w) as usize;
                if can_be_sampled[ni] {
                    let base = ni * 4;
                    r += pixels[base] as u32;
                    g += pixels[base + 1] as u32;
                    b += pixels[base + 2] as u32;
                    count += 1;
                } else if !visited[ni] {
                    visited.set(ni, true);
                    next.push_back(ni as u32);
                }
            }

            #[allow(clippy::manual_checked_ops)] // count > 0 guard makes this safe
            if count > 0 {
                let base = i * 4;
                pixels[base] = (r / count) as u8;
                pixels[base + 1] = (g / count) as u8;
                pixels[base + 2] = (b / count) as u8;
                // pixels[base + 3] stays 0 — alpha is never written
            }
        }

        // Mark everything processed in this wave as samplable for the next.
        for &flat in &current {
            can_be_sampled.set(flat as usize, true);
        }

        std::mem::swap(&mut current, &mut next);
        next.clear();
    }
}

const OFFSETS_8: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bleed_does_not_alter_opaque_pixels() {
        let mut img = RgbaImage::new(3, 3);
        img.put_pixel(1, 1, Rgba([255, 0, 0, 255]));
        alpha_bleed(&mut img);
        assert_eq!(img.get_pixel(1, 1), &Rgba([255, 0, 0, 255]));
    }

    #[test]
    fn test_bleed_propagates_color_to_transparent_border() {
        let mut img = RgbaImage::new(3, 3);
        img.put_pixel(1, 1, Rgba([0, 128, 255, 255]));
        alpha_bleed(&mut img);
        for &(x, y) in &[(0u32, 1u32), (2, 1), (1, 0), (1, 2)] {
            let p = img.get_pixel(x, y);
            assert_eq!(p[3], 0, "alpha should remain 0 at ({x},{y})");
            assert!(
                p[0] > 0 || p[1] > 0 || p[2] > 0,
                "bled color expected at ({x},{y})"
            );
        }
    }

    #[test]
    fn test_fully_transparent_image_unchanged() {
        let mut img = RgbaImage::new(4, 4);
        alpha_bleed(&mut img);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(img.get_pixel(x, y), &Rgba([0, 0, 0, 0]));
            }
        }
    }

    #[test]
    fn test_fully_opaque_image_unchanged() {
        let mut img = RgbaImage::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                img.put_pixel(x, y, Rgba([100, 150, 200, 255]));
            }
        }
        alpha_bleed(&mut img);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(img.get_pixel(x, y), &Rgba([100, 150, 200, 255]));
            }
        }
    }

    #[test]
    fn test_zero_size_image_does_not_panic() {
        let mut img = RgbaImage::new(0, 0);
        alpha_bleed(&mut img);
    }

    #[test]
    fn test_bleed_alpha_stays_zero() {
        let mut img = RgbaImage::new(5, 5);
        img.put_pixel(2, 2, Rgba([255, 255, 255, 255]));
        alpha_bleed(&mut img);
        for y in 0..5 {
            for x in 0..5 {
                if x == 2 && y == 2 {
                    continue;
                }
                assert_eq!(img.get_pixel(x, y)[3], 0, "alpha must stay 0 at ({x},{y})");
            }
        }
    }
}
