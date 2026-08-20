//! Expands opaque pixel colors into fully-transparent border pixels.
//! This prevents dark-fringe artifacts when images are scaled or filtered,
//! particularly visible at spritesheet sprite boundaries.
//!
//! Algorithm: BFS outward from every opaque pixel, averaging neighbor colors
//! into each transparent pixel it reaches. Alpha stays 0 — only RGB is written.
//!
//! Optimizations:
//! - `bit_vec::BitVec` for compact boolean storage
//! - `VecDeque` for wave queues
//! - 4-neighbor fast path, 8-neighbor fallback for correctness
//! - thread-local buffer reuse: each Rayon worker processes many images, so
//!   the BitVecs/queues are grown once per thread instead of re-allocated per
//!   image.

use bit_vec::BitVec;
#[cfg(test)]
use image::Rgba;
use image::RgbaImage;
use std::cell::RefCell;
use std::collections::VecDeque;

/// 4-neighbor offsets (cardinal directions) — checked first for speed.
const OFFSETS_4: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

/// 8-neighbor offsets (including diagonals) — fallback for correctness.
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

/// Reusable per-thread buffers for the wavefront algorithm.
struct BleedBuffers {
    can_be_sampled: Option<BitVec>,
    visited: Option<BitVec>,
    current_wave: VecDeque<u32>,
    next_wave: VecDeque<u32>,
    /// Pixels written in the current wave, marked samplable after the wave drains.
    written_this_wave: Vec<u32>,
}

thread_local! {
    static BUFFERS: RefCell<BleedBuffers> = const {
        RefCell::new(BleedBuffers {
            can_be_sampled: None,
            visited: None,
            current_wave: VecDeque::new(),
            next_wave: VecDeque::new(),
            written_this_wave: Vec::new(),
        })
    };
}

pub fn alpha_bleed(img: &mut RgbaImage) {
    BUFFERS.with(|cell| {
        let mut buffers = cell.borrow_mut();
        run_alpha_bleed(img, &mut buffers);
    });
}

fn run_alpha_bleed(img: &mut RgbaImage, b: &mut BleedBuffers) {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return;
    }

    let pixel_count = (width * height) as usize;
    // Wavefront queues are bounded by the perimeter, not the total area.
    let wave_capacity = ((width + height) * 2).max(256) as usize;

    // (Re)use buffers: create once per thread, then grow to fit and reset.
    let can_be_sampled = b
        .can_be_sampled
        .get_or_insert_with(|| BitVec::from_elem(pixel_count, false));
    let visited = b
        .visited
        .get_or_insert_with(|| BitVec::from_elem(pixel_count, false));
    if can_be_sampled.len() < pixel_count {
        can_be_sampled.grow(pixel_count - can_be_sampled.len(), false);
    }
    can_be_sampled.fill(false);
    if visited.len() < pixel_count {
        visited.grow(pixel_count - visited.len(), false);
    }
    visited.fill(false);
    b.current_wave.clear();
    b.next_wave.clear();
    b.written_this_wave.clear();
    if b.current_wave.capacity() < wave_capacity {
        b.current_wave
            .reserve(wave_capacity - b.current_wave.capacity());
    }
    if b.next_wave.capacity() < wave_capacity {
        b.next_wave.reserve(wave_capacity - b.next_wave.capacity());
    }
    if b.written_this_wave.capacity() < wave_capacity {
        b.written_this_wave
            .reserve(wave_capacity - b.written_this_wave.capacity());
    }

    let pixels = img.as_raw();

    // Init pass: mark opaque pixels
    for index in 0..pixel_count {
        let alpha = pixels[index * 4 + 3];
        if alpha != 0 {
            can_be_sampled.set(index, true);
            visited.set(index, true);
        }
    }

    // Seed: transparent pixels adjacent to opaque (check 4-neighbor first for speed)
    for y in 0..height {
        for x in 0..width {
            let index = (x + y * width) as usize;
            if can_be_sampled.get(index).unwrap_or(false) {
                continue;
            }
            let mut borders_opaque = false;
            for &(dx, dy) in &OFFSETS_4 {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32 {
                    let nidx = (nx as u32 + ny as u32 * width) as usize;
                    if can_be_sampled.get(nidx).unwrap_or(false) {
                        borders_opaque = true;
                        break;
                    }
                }
            }
            if !borders_opaque {
                for &(dx, dy) in &OFFSETS_8[4..] {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32 {
                        let nidx = (nx as u32 + ny as u32 * width) as usize;
                        if can_be_sampled.get(nidx).unwrap_or(false) {
                            borders_opaque = true;
                            break;
                        }
                    }
                }
            }
            if borders_opaque {
                visited.set(index, true);
                b.current_wave.push_back(index as u32);
            }
        }
    }

    // Wave-front BFS with double-buffered queues
    let pixels = img.as_mut();

    while !b.current_wave.is_empty() {
        b.written_this_wave.clear();

        while let Some(flat_index) = b.current_wave.pop_front() {
            let index = flat_index as usize;
            let x = flat_index % width;
            let y = flat_index / width;

            let mut red_sum = 0u32;
            let mut green_sum = 0u32;
            let mut blue_sum = 0u32;
            let mut sample_count = 0u32;

            // Try 4-neighbor first (faster, cache-friendly)
            for &(dx, dy) in &OFFSETS_4 {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                    continue;
                }
                let nidx = (nx as u32 + ny as u32 * width) as usize;
                if can_be_sampled.get(nidx).unwrap_or(false) {
                    let base = nidx * 4;
                    red_sum += pixels[base] as u32;
                    green_sum += pixels[base + 1] as u32;
                    blue_sum += pixels[base + 2] as u32;
                    sample_count += 1;
                } else if !visited.get(nidx).unwrap_or(false) {
                    visited.set(nidx, true);
                    b.next_wave.push_back(nidx as u32);
                }
            }

            // Fall back to diagonals if needed
            if sample_count == 0 {
                for &(dx, dy) in &OFFSETS_8[4..] {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                        continue;
                    }
                    let nidx = (nx as u32 + ny as u32 * width) as usize;
                    if can_be_sampled.get(nidx).unwrap_or(false) {
                        let base = nidx * 4;
                        red_sum += pixels[base] as u32;
                        green_sum += pixels[base + 1] as u32;
                        blue_sum += pixels[base + 2] as u32;
                        sample_count += 1;
                    } else if !visited.get(nidx).unwrap_or(false) {
                        visited.set(nidx, true);
                        b.next_wave.push_back(nidx as u32);
                    }
                }
            }

            #[allow(clippy::manual_checked_ops)] // sample_count > 0 guard makes this safe
            if sample_count > 0 {
                let base = index * 4;
                pixels[base] = (red_sum / sample_count) as u8;
                pixels[base + 1] = (green_sum / sample_count) as u8;
                pixels[base + 2] = (blue_sum / sample_count) as u8;
                // Record this pixel so we can mark it as samplable after the
                // wave is fully drained — the deque is empty at that point.
                b.written_this_wave.push(flat_index);
            }
        }

        // Mark the pixels written in this wave as samplable so the next wave
        // can use their colours as bleed sources.
        for &flat_index in &b.written_this_wave {
            can_be_sampled.set(flat_index as usize, true);
        }

        // Swap queues for next wave
        std::mem::swap(&mut b.current_wave, &mut b.next_wave);
        b.next_wave.clear();
    }
}

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

    #[test]
    fn test_large_image_performance() {
        // Smoke test for larger images
        let mut img = RgbaImage::new(512, 512);
        img.put_pixel(256, 256, Rgba([255, 0, 0, 255]));
        alpha_bleed(&mut img);
        // Center should remain unchanged
        assert_eq!(img.get_pixel(256, 256), &Rgba([255, 0, 0, 255]));
        // Neighbors should be bled
        let p = img.get_pixel(255, 256);
        assert!(p[0] > 0 || p[1] > 0 || p[2] > 0);
    }

    #[test]
    fn test_buffer_reuse_keeps_results_consistent() {
        // Two calls in a row on the same thread exercise the thread-local
        // buffer reuse path; results must match the fresh-path invariants.
        let mut img1 = RgbaImage::new(3, 3);
        img1.put_pixel(1, 1, Rgba([0, 128, 255, 255]));
        alpha_bleed(&mut img1);

        let mut img2 = RgbaImage::new(5, 5);
        img2.put_pixel(2, 2, Rgba([255, 255, 255, 255]));
        alpha_bleed(&mut img2);

        // Second (larger) image still bleeds correctly.
        assert_eq!(img2.get_pixel(2, 2), &Rgba([255, 255, 255, 255]));
        assert_eq!(img2.get_pixel(1, 2)[3], 0);
        assert!(img2.get_pixel(1, 2)[0] > 0 || img2.get_pixel(1, 2)[1] > 0);

        // And a third, smaller image on the same thread (buffers shrink back).
        let mut img3 = RgbaImage::new(3, 3);
        img3.put_pixel(1, 1, Rgba([0, 128, 255, 255]));
        alpha_bleed(&mut img3);
        for &(x, y) in &[(0u32, 1u32), (2, 1), (1, 0), (1, 2)] {
            let p = img3.get_pixel(x, y);
            assert_eq!(p[3], 0, "alpha should remain 0 at ({x},{y})");
            assert!(p[0] > 0 || p[1] > 0 || p[2] > 0);
        }
    }
}
