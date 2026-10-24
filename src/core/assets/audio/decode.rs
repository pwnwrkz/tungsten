//! Audio decoding to raw PCM via symphonia.

use anyhow::{Context, Result, anyhow, bail};
use symphonia::core::codecs::audio::{AudioDecoderOptions, CODEC_ID_NULL_AUDIO};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Decoded PCM audio: interleaved 32-bit float samples.
pub struct DecodedAudio {
    /// Interleaved samples in the range `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
    /// Number of channels.
    pub channels: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

impl DecodedAudio {
    /// Number of frames (samples per channel).
    #[allow(dead_code)]
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }
}

/// Decode any supported audio container (WAV, MP3, M4A/AAC, OGG, FLAC)
/// to interleaved f32 PCM via symphonia.
///
/// `ext` is the source file extension, used as a probe hint.
pub fn decode(data: &[u8], ext: &str) -> Result<DecodedAudio> {
    let cursor = std::io::Cursor::new(data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .context("Failed to probe audio container format")?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("No decodable audio track found"))?;
    let track_id = track.id;

    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .filter(|p| p.codec != CODEC_ID_NULL_AUDIO)
        .ok_or_else(|| anyhow!("No decodable audio track found"))?
        .clone();

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .context("Failed to create audio decoder")?;

    let mut samples: Vec<f32> = Vec::new();
    let mut channels: u32 = 0;
    let mut sample_rate: u32 = 0;

    loop {
        let packet = match format
            .next_packet()
            .context("Failed to read audio packet")?
        {
            Some(p) => p,
            // End of stream.
            None => break,
        };

        if packet.track_id != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Recoverable decode errors: skip the packet.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e).context("Failed to decode audio packet"),
        };

        if channels == 0 {
            channels = decoded.spec().channels().count() as u32;
            sample_rate = decoded.spec().rate();
        }

        // `copy_to_vec_interleaved` overwrites from index 0, so grow the
        // output by this packet's sample count and fill only the tail —
        // the 0.5.x `SampleBuffer` + `extend_from_slice` append behavior.
        let n = decoded.samples_interleaved();
        let start = samples.len();
        samples.resize(start + n, 0.0);
        decoded.copy_to_slice_interleaved(&mut samples[start..]);
    }

    if channels == 0 || sample_rate == 0 || samples.is_empty() {
        bail!("Audio stream contained no decodable samples");
    }

    Ok(DecodedAudio {
        samples,
        channels,
        sample_rate,
    })
}
