//! Expands opaque pixel colors into fully-transparent border pixels.
//! This prevents dark-fringe artifacts when images are scaled or filtered,
//! particularly visible at spritesheet sprite boundaries.
//!
//! Algorithm: BFS outward from every opaque pixel, averaging neighbor colors
//! into each transparent pixel it reaches. Alpha stays 0 — only RGB is written.

use std::collections::VecDeque;

use bit_vec::BitVec;
use image::{Rgba, RgbaImage};

const DIRECTIONS: &[(i32, i32)] = &[
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

pub fn alpha_bleed(img: &mut RgbaImage) {
    let (w, h) = img.dimensions();
    let len = (w * h) as usize;

    let mut can_be_sampled = BitVec::from_elem(len, false);
    let mut visited = BitVec::from_elem(len, false);

    let idx = |x: u32, y: u32| (x + y * w) as usize;

    let adjacent = |x: u32, y: u32| -> Vec<(u32, u32)> {
        DIRECTIONS
            .iter()
            .filter_map(|&(dx, dy)| {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 {
                    Some((nx as u32, ny as u32))
                } else {
                    None
                }
            })
            .collect()
    };

    let mut queue: VecDeque<(u32, u32)> = VecDeque::new();

    for y in 0..h {
        for x in 0..w {
            if img.get_pixel(x, y)[3] != 0 {
                can_be_sampled.set(idx(x, y), true);
                visited.set(idx(x, y), true);
            } else {
                let borders_opaque = adjacent(x, y)
                    .iter()
                    .any(|&(nx, ny)| img.get_pixel(nx, ny)[3] != 0);
                if borders_opaque {
                    visited.set(idx(x, y), true);
                    queue.push_back((x, y));
                }
            }
        }
    }

    while !queue.is_empty() {
        let wave_len = queue.len();
        let mut processed: Vec<(u32, u32)> = Vec::with_capacity(wave_len);

        for _ in 0..wave_len {
            let (x, y) = queue.pop_front().unwrap();

            let mut r = 0u32;
            let mut g = 0u32;
            let mut b = 0u32;
            let mut count = 0u32;

            for (nx, ny) in adjacent(x, y) {
                if can_be_sampled[idx(nx, ny)] {
                    let p = img.get_pixel(nx, ny);
                    r += p[0] as u32;
                    g += p[1] as u32;
                    b += p[2] as u32;
                    count += 1;
                } else if !visited[idx(nx, ny)] {
                    visited.set(idx(nx, ny), true);
                    queue.push_back((nx, ny));
                }
            }

            if count > 0 {
                img.put_pixel(
                    x,
                    y,
                    Rgba([(r / count) as u8, (g / count) as u8, (b / count) as u8, 0]),
                );
            }

            processed.push((x, y));
        }

        for (x, y) in processed {
            can_be_sampled.set(idx(x, y), true);
        }
    }
}

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
}
