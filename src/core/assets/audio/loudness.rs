//! EBU R128 loudness measurement and normalization gain (ebur128).

use anyhow::{Context, Result};
use ebur128::{EbuR128, Mode};

use crate::log;

use super::decode::DecodedAudio;

/// Resolved loudness normalization settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedNormalizeOptions {
    /// Integrated loudness target in LUFS.
    pub target_lufs: f64,
    /// True peak ceiling in dBTP. Gain is reduced so the output's true peak
    /// never exceeds this, even if that means missing the loudness target.
    pub true_peak_ceiling: f64,
}

/// Measured loudness statistics for a decoded stream.
#[derive(Debug, Clone, Copy)]
pub struct LoudnessStats {
    /// Integrated loudness in LUFS per EBU R128 / ITU-R BS.1770-4
    /// (K-weighted, gated at -70 LUFS absolute and -10 LU relative).
    pub integrated_lufs: f64,
    /// Highest true peak across all channels, linear amplitude.
    pub true_peak: f64,
}

/// Measure integrated loudness and true peak of decoded PCM.
pub fn measure(pcm: &DecodedAudio) -> Result<LoudnessStats> {
    let mut meter = EbuR128::new(pcm.channels, pcm.sample_rate, Mode::I | Mode::TRUE_PEAK)
        .context("Failed to create loudness meter")?;
    meter
        .add_frames_f32(&pcm.samples)
        .context("Failed to feed samples to loudness meter")?;

    let integrated_lufs = meter
        .loudness_global()
        .context("Failed to compute integrated loudness")?;

    let mut true_peak: f64 = 0.0;
    for ch in 0..pcm.channels {
        let peak = meter.true_peak(ch).context("Failed to compute true peak")?;
        true_peak = true_peak.max(peak);
    }

    Ok(LoudnessStats {
        integrated_lufs,
        true_peak,
    })
}

/// Apply gain so `pcm` hits the target integrated loudness, limited by the
/// true peak ceiling. Returns the gain applied in dB, or `None` when the
/// stream is effectively silent (below the -70 LUFS absolute gate) and left
/// untouched.
pub fn normalize(
    pcm: &mut DecodedAudio,
    options: &ResolvedNormalizeOptions,
) -> Result<Option<f64>> {
    let stats = measure(pcm)?;

    // loudness_global returns -inf for fully gated (silent) input.
    if !stats.integrated_lufs.is_finite() || stats.integrated_lufs < -70.0 {
        return Ok(None);
    }

    let desired_gain_db = options.target_lufs - stats.integrated_lufs;
    let mut gain = db_to_linear(desired_gain_db);

    // Cap the gain so the true peak stays under the ceiling.
    if stats.true_peak > 0.0 {
        let peak_limit = db_to_linear(options.true_peak_ceiling) / stats.true_peak;
        if gain > peak_limit {
            log!(
                debug,
                "Gain limited by true peak ceiling: {:+.2} dB requested, {:+.2} dB applied",
                desired_gain_db,
                linear_to_db(peak_limit)
            );
            gain = peak_limit;
        }
    }

    for sample in pcm.samples.iter_mut() {
        *sample = (*sample * gain as f32).clamp(-1.0, 1.0);
    }

    Ok(Some(linear_to_db(gain)))
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

fn linear_to_db(linear: f64) -> f64 {
    20.0 * linear.log10()
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::assets::audio::transcode::tests::sine_samples;

    fn decoded(samples: Vec<f32>) -> DecodedAudio {
        DecodedAudio {
            samples,
            channels: 2,
            sample_rate: 44100,
        }
    }

    fn default_options() -> ResolvedNormalizeOptions {
        ResolvedNormalizeOptions {
            target_lufs: -14.0,
            true_peak_ceiling: -1.0,
        }
    }

    #[test]
    fn test_measure_full_scale_sine() {
        // A full-scale 440 Hz stereo sine measures around -0.6 LUFS
        // (K-weighting boosts nothing at 440 Hz; both channels sum +3 dB
        // over a single-channel -3.01 dBFS RMS reading).
        let pcm = decoded(sine_samples(1.0, 44100));
        let stats = measure(&pcm).unwrap();
        assert!(
            (stats.integrated_lufs - (-0.7)).abs() < 1.0,
            "integrated = {}",
            stats.integrated_lufs
        );
        assert!(
            (stats.true_peak - 1.0).abs() < 0.05,
            "true peak = {}",
            stats.true_peak
        );
    }

    #[test]
    fn test_normalize_quiet_audio_boosts_to_target() {
        // -40 dBFS sine: far below target, needs a large boost.
        let mut pcm = decoded(sine_samples(0.01, 44100));
        let applied = normalize(&mut pcm, &default_options()).unwrap();
        assert!(applied.unwrap() > 20.0, "applied = {:?}", applied);

        let stats = measure(&pcm).unwrap();
        assert!(
            (stats.integrated_lufs - (-14.0)).abs() < 0.5,
            "normalized integrated = {}",
            stats.integrated_lufs
        );
    }

    #[test]
    fn test_normalize_loud_audio_attenuates_to_target() {
        let mut pcm = decoded(sine_samples(0.9, 44100));
        let applied = normalize(&mut pcm, &default_options()).unwrap();
        assert!(applied.unwrap() < 0.0, "applied = {:?}", applied);

        let stats = measure(&pcm).unwrap();
        assert!(
            (stats.integrated_lufs - (-14.0)).abs() < 0.5,
            "normalized integrated = {}",
            stats.integrated_lufs
        );
    }

    #[test]
    fn test_normalize_respects_true_peak_ceiling() {
        // Asking for a hot target must not push the true peak over the ceiling.
        let mut pcm = decoded(sine_samples(0.5, 44100));
        let options = ResolvedNormalizeOptions {
            target_lufs: 0.0,
            true_peak_ceiling: -1.0,
        };
        normalize(&mut pcm, &options).unwrap();

        let stats = measure(&pcm).unwrap();
        let ceiling_linear = 10f64.powf(-1.0 / 20.0);
        assert!(
            stats.true_peak <= ceiling_linear * 1.01,
            "true peak = {}",
            stats.true_peak
        );
        // Loudness target intentionally missed.
        assert!(stats.integrated_lufs < -1.0);
    }

    #[test]
    fn test_normalize_skips_silence() {
        let mut pcm = decoded(vec![0.0; 88200]);
        let before = pcm.samples.clone();
        let applied = normalize(&mut pcm, &default_options()).unwrap();
        assert!(applied.is_none());
        assert_eq!(pcm.samples, before, "silent samples must be untouched");
    }
}
