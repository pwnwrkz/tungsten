//! Full audio pipeline: decode → (normalize) → re-encode to OGG Vorbis.

use anyhow::{Context, Result};

use crate::log;

use super::decode::decode;
use super::encode::{VorbisProfile, encode_ogg};
use super::loudness::{ResolvedNormalizeOptions, normalize};

/// Resolved audio processing settings for an input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedAudioOptions {
    /// Vorbis encoder profile used when re-encoding.
    pub profile: VorbisProfile,
    /// Loudness normalization settings, if enabled.
    pub normalize: Option<ResolvedNormalizeOptions>,
}

impl ResolvedAudioOptions {
    /// Stable string identifying these settings. Mixed into lockfile cache
    /// keys so changing the settings re-processes and re-uploads, while
    /// unchanged sources skip the decode/encode round-trip entirely.
    pub fn fingerprint(&self) -> String {
        let profile = match self.profile {
            VorbisProfile::Quality(q) => format!("q{}", q),
            VorbisProfile::Bitrate(kbps) => format!("b{}", kbps),
        };
        match &self.normalize {
            Some(n) => format!("{};n{},{}", profile, n.target_lufs, n.true_peak_ceiling),
            None => profile,
        }
    }
}

/// Transcode an audio file of any supported container to OGG Vorbis,
/// optionally normalizing loudness on the way through.
///
/// Without normalization, sources that are already OGG are passed through
/// untouched (`Ok(None)`) to avoid a pointless lossy re-encode generation.
/// With normalization enabled, every source — OGG included — is decoded,
/// gain-adjusted to the target loudness and re-encoded. Everything else
/// (WAV, MP3, M4A/AAC, FLAC) is decoded to PCM via symphonia and re-encoded
/// with vorbis_rs — pure Rust, no ffmpeg.
pub fn transcode_to_ogg(
    data: &[u8],
    ext: &str,
    options: &ResolvedAudioOptions,
) -> Result<Option<Vec<u8>>> {
    if ext.eq_ignore_ascii_case("ogg") && options.normalize.is_none() {
        return Ok(None);
    }

    let mut pcm = decode(data, ext).context("Failed to decode audio for transcoding")?;

    if let Some(normalize_options) = &options.normalize {
        match normalize(&mut pcm, normalize_options).context("Failed to normalize loudness")? {
            Some(gain_db) => log!(
                debug,
                "Normalized to {} LUFS ({:+.2} dB gain)",
                normalize_options.target_lufs,
                gain_db
            ),
            None => log!(debug, "Skipped normalization: stream is silent"),
        }
    }

    let ogg = encode_ogg(&pcm.samples, pcm.channels, pcm.sample_rate, options.profile)
        .context("Failed to encode OGG Vorbis")?;
    Ok(Some(ogg))
}

// Tests

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a minimal PCM16 WAV file in memory.
    pub(crate) fn make_wav(samples: &[f32], channels: u16, sample_rate: u32) -> Vec<u8> {
        let pcm: Vec<u8> = samples
            .iter()
            .flat_map(|&s| {
                let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                v.to_le_bytes()
            })
            .collect();

        let byte_rate = sample_rate * channels as u32 * 2;
        let block_align = channels * 2;
        let data_len = pcm.len() as u32;

        let mut wav = Vec::with_capacity(44 + pcm.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(&pcm);
        wav
    }

    /// One second of 440 Hz stereo sine at the given amplitude.
    pub(crate) fn sine_samples(amplitude: f32, sample_rate: u32) -> Vec<f32> {
        (0..sample_rate)
            .flat_map(|i| {
                let t = i as f32 / sample_rate as f32;
                let s = amplitude * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                [s, s]
            })
            .collect()
    }

    fn default_options() -> ResolvedAudioOptions {
        ResolvedAudioOptions {
            profile: VorbisProfile::Quality(5.0),
            normalize: None,
        }
    }

    #[test]
    fn test_wav_transcodes_to_valid_ogg() {
        let samples = sine_samples(0.5, 44100);
        let wav = make_wav(&samples, 2, 44100);

        let ogg = transcode_to_ogg(&wav, "wav", &default_options())
            .unwrap()
            .expect("wav should be transcoded");

        // OGG container magic.
        assert_eq!(&ogg[..4], b"OggS");

        // Round-trip: our own decoder should read the result back.
        let decoded = decode(&ogg, "ogg").unwrap();
        assert_eq!(decoded.channels, 2);
        assert_eq!(decoded.sample_rate, 44100);
        // Vorbis priming/padding shifts the exact count; expect roughly 1s.
        let frames = decoded.frames() as f64;
        assert!((frames - 44100.0).abs() < 4410.0, "frames = {}", frames);
    }

    #[test]
    fn test_ogg_source_is_passed_through() {
        let samples = sine_samples(0.5, 44100);
        let wav = make_wav(&samples, 2, 44100);
        let ogg = transcode_to_ogg(&wav, "wav", &default_options())
            .unwrap()
            .unwrap();

        let result = transcode_to_ogg(&ogg, "ogg", &default_options()).unwrap();
        assert!(result.is_none(), "ogg sources should not be re-encoded");
    }

    #[test]
    fn test_bitrate_profile_produces_smaller_stream_than_high_quality() {
        let samples = sine_samples(0.5, 44100);
        let wav = make_wav(&samples, 2, 44100);

        let small = transcode_to_ogg(
            &wav,
            "wav",
            &ResolvedAudioOptions {
                profile: VorbisProfile::Bitrate(64),
                normalize: None,
            },
        )
        .unwrap()
        .unwrap();
        let large = transcode_to_ogg(
            &wav,
            "wav",
            &ResolvedAudioOptions {
                profile: VorbisProfile::Quality(10.0),
                normalize: None,
            },
        )
        .unwrap()
        .unwrap();

        assert!(small.len() < large.len());
    }

    #[test]
    fn test_garbage_input_errors() {
        let result = transcode_to_ogg(&[0u8; 64], "wav", &default_options());
        assert!(result.is_err());
    }

    #[test]
    fn test_normalization_forces_ogg_reencode() {
        use crate::core::assets::audio::loudness::ResolvedNormalizeOptions;

        let samples = sine_samples(0.5, 44100);
        let wav = make_wav(&samples, 2, 44100);
        let ogg = transcode_to_ogg(&wav, "wav", &default_options())
            .unwrap()
            .unwrap();

        let options = ResolvedAudioOptions {
            profile: VorbisProfile::Quality(5.0),
            normalize: Some(ResolvedNormalizeOptions {
                target_lufs: -14.0,
                true_peak_ceiling: -1.0,
            }),
        };
        let result = transcode_to_ogg(&ogg, "ogg", &options).unwrap();
        assert!(
            result.is_some(),
            "normalization requires re-encoding even for ogg sources"
        );
    }

    #[test]
    fn test_transcode_with_normalization_hits_target() {
        use crate::core::assets::audio::loudness::{ResolvedNormalizeOptions, measure};

        // A quiet source should come out at the loudness target.
        let samples = sine_samples(0.05, 44100);
        let wav = make_wav(&samples, 2, 44100);

        let options = ResolvedAudioOptions {
            profile: VorbisProfile::Quality(5.0),
            normalize: Some(ResolvedNormalizeOptions {
                target_lufs: -14.0,
                true_peak_ceiling: -1.0,
            }),
        };
        let ogg = transcode_to_ogg(&wav, "wav", &options).unwrap().unwrap();

        let decoded = decode(&ogg, "ogg").unwrap();
        let stats = measure(&decoded).unwrap();
        // Lossy round-trip wobbles the measurement slightly.
        assert!(
            (stats.integrated_lufs - (-14.0)).abs() < 1.0,
            "integrated = {}",
            stats.integrated_lufs
        );
    }
}
