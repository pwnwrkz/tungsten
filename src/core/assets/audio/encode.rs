//! OGG Vorbis encoding via vorbis_rs (quality or bitrate profiles).

use std::num::{NonZeroU8, NonZeroU32};

use anyhow::{Context, Result, bail};
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

/// Frames per block fed to the Vorbis encoder.
const ENCODE_BLOCK_FRAMES: usize = 4096;

/// How the Vorbis encoder allocates bits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VorbisProfile {
    /// libvorbis quality scale, `-1.0..=10.0`. 5.0 is roughly 160 kbps stereo.
    Quality(f32),
    /// Target VBR bitrate in kbps.
    Bitrate(u32),
}

/// Encode interleaved f32 PCM to an OGG Vorbis stream.
#[allow(clippy::needless_range_loop)]
pub fn encode_ogg(
    samples: &[f32],
    channels: u32,
    sample_rate: u32,
    profile: VorbisProfile,
) -> Result<Vec<u8>> {
    let Some(rate) = NonZeroU32::new(sample_rate) else {
        bail!("Sample rate must be non-zero");
    };
    let channel_count = u8::try_from(channels)
        .ok()
        .and_then(NonZeroU8::new)
        .with_context(|| format!("Unsupported channel count {}", channels))?;

    let strategy = match profile {
        VorbisProfile::Quality(q) => VorbisBitrateManagementStrategy::QualityVbr {
            // Map the familiar -1..10 libvorbis scale onto vorbis_rs's -0.1..1.0.
            target_quality: (q.clamp(-1.0, 10.0)) / 10.0,
        },
        VorbisProfile::Bitrate(kbps) => VorbisBitrateManagementStrategy::Vbr {
            target_bitrate: NonZeroU32::new(kbps.saturating_mul(1000))
                .context("Bitrate must be non-zero")?,
        },
    };

    let mut output: Vec<u8> = Vec::new();
    let mut encoder = VorbisEncoderBuilder::new(rate, channel_count, &mut output)
        .context("Failed to configure Vorbis encoder")?
        .bitrate_management_strategy(strategy)
        .build()
        .context("Failed to initialize Vorbis encoder")?;

    let ch = channels as usize;
    // Pre-allocated planar buffers reused across blocks. Each plane is resized
    // to the exact frame count at the start of every block so we can write via
    // direct indexing in the inner loop without the branchprediction overhead of
    // Vec::push.
    let mut planar: Vec<Vec<f32>> = vec![Vec::with_capacity(ENCODE_BLOCK_FRAMES); ch];

    for block in samples.chunks(ENCODE_BLOCK_FRAMES * ch) {
        let frame_count = block.len() / ch;
        for plane in planar.iter_mut() {
            plane.resize(frame_count, 0.0);
        }
        for frame_idx in 0..frame_count {
            let frame_start = frame_idx * ch;
            for c in 0..ch {
                planar[c][frame_idx] = block[frame_start + c];
            }
        }
        encoder
            .encode_audio_block(&planar)
            .context("Failed to encode audio block")?;
    }

    encoder.finish().context("Failed to finalize OGG stream")?;

    Ok(output)
}
