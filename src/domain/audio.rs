//! Rendered speech audio: 24 kHz mono 16-bit little-endian PCM, served as WAV, with a
//! duration and 64 waveform peaks computed the same way for every render.

/// Sample rate of rendered speech (what Deepgram's Voice Agent returns for `linear16`).
pub const SAMPLE_RATE: u32 = 24_000;
/// Bytes per sample (16-bit mono).
const BYTES_PER_SAMPLE: u32 = 2;
/// Length of the canonical WAV header.
pub const WAV_HEADER_BYTES: usize = 44;
/// How many peaks describe one rendering's waveform.
pub const PEAK_COUNT: usize = 64;

/// A canonical PCM WAV file around raw 16-bit mono PCM. An odd trailing byte (half a
/// sample) is dropped.
#[must_use]
pub fn wav_from_pcm(pcm: &[u8]) -> Vec<u8> {
    let data = &pcm[..pcm.len() - pcm.len() % 2];
    let mut wav = Vec::with_capacity(WAV_HEADER_BYTES + data.len());
    wav.extend_from_slice(&wav_header(data.len()));
    wav.extend_from_slice(data);
    wav
}

/// [`wav_from_pcm`] that reuses the PCM's own buffer: the header is written in front of the
/// samples instead of copying them, so a long rendering is held in memory once, not twice.
#[must_use]
pub fn wav_from_pcm_buffer(mut pcm: Vec<u8>) -> Vec<u8> {
    pcm.truncate(pcm.len() - pcm.len() % 2);
    let header = wav_header(pcm.len());
    pcm.splice(0..0, header);
    pcm
}

fn wav_header(data_bytes: usize) -> [u8; WAV_HEADER_BYTES] {
    let data_length = u32::try_from(data_bytes).unwrap_or(u32::MAX - 36);
    let mut wav = Vec::with_capacity(WAV_HEADER_BYTES);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_length).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * BYTES_PER_SAMPLE).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_length.to_le_bytes());
    let mut header = [0; WAV_HEADER_BYTES];
    header.copy_from_slice(&wav);
    header
}

/// Milliseconds of audio in raw PCM, rounded to the nearest millisecond.
#[must_use]
pub fn duration_milliseconds(pcm_bytes: usize) -> u32 {
    let samples = u64::try_from(pcm_bytes / 2).unwrap_or(u64::MAX);
    let milliseconds = (samples * 1_000 + u64::from(SAMPLE_RATE) / 2) / u64::from(SAMPLE_RATE);
    u32::try_from(milliseconds).unwrap_or(u32::MAX)
}

/// 64 values from 0 to 100: the RMS loudness of 64 equal windows of the PCM, relative to
/// the loudest window (silence is all zeros). Shorter audio than 64 samples pads with zeros.
#[must_use]
pub fn peaks(pcm: &[u8]) -> Vec<i16> {
    let samples: Vec<f64> = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f64::from(i16::from_le_bytes(*pair)))
        .collect();
    let mut rms = vec![0.0_f64; PEAK_COUNT];
    if !samples.is_empty() {
        let window = samples.len().div_ceil(PEAK_COUNT);
        for (index, chunk) in samples.chunks(window).enumerate().take(PEAK_COUNT) {
            let energy: f64 = chunk.iter().map(|sample| sample * sample).sum();
            #[allow(
                clippy::cast_precision_loss,
                reason = "a window holds at most a few million samples"
            )]
            let mean = energy / chunk.len() as f64;
            rms[index] = mean.sqrt();
        }
    }
    let loudest = rms.iter().copied().fold(0.0_f64, f64::max);
    rms.into_iter()
        .map(|value| {
            if loudest < 1.0 {
                0
            } else {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "the value is clamped to 0..=100 first"
                )]
                let peak = (value / loudest * 100.0).round().clamp(0.0, 100.0) as i16;
                peak
            }
        })
        .collect()
}

/// Whether `bytes` start like a canonical WAV of rendered speech (what DM stores).
#[must_use]
pub fn is_wav(bytes: &[u8]) -> bool {
    bytes.len() >= WAV_HEADER_BYTES && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

#[cfg(test)]
mod tests {
    use super::{
        PEAK_COUNT, duration_milliseconds, is_wav, peaks, wav_from_pcm, wav_from_pcm_buffer,
    };

    fn tone(samples: usize, amplitude: i16) -> Vec<u8> {
        (0..samples)
            .flat_map(|index| {
                let value = if index % 2 == 0 {
                    amplitude
                } else {
                    -amplitude
                };
                value.to_le_bytes()
            })
            .collect()
    }

    #[test]
    fn wav_headers_describe_24khz_mono_16_bit_pcm() {
        let pcm = tone(24_000, 1_000);
        let wav = wav_from_pcm(&pcm);
        assert!(is_wav(&wav));
        assert_eq!(wav.len(), 44 + pcm.len());
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            24_000
        );
        assert_eq!(
            u32::from_le_bytes([wav[28], wav[29], wav[30], wav[31]]),
            48_000
        );
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16);
        assert_eq!(
            u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]),
            u32::try_from(pcm.len()).unwrap_or_default()
        );
        assert_eq!(
            wav_from_pcm_buffer(pcm.clone()),
            wav,
            "the same file, built in the PCM's own buffer"
        );
        assert_eq!(wav_from_pcm_buffer(vec![1, 2, 3]).len(), 46);
        assert_eq!(
            wav_from_pcm(&[1, 2, 3]).len(),
            46,
            "half a sample is dropped"
        );
    }

    #[test]
    fn durations_come_from_the_sample_count() {
        assert_eq!(duration_milliseconds(48_000), 1_000);
        assert_eq!(duration_milliseconds(218_880), 4_560);
        assert_eq!(duration_milliseconds(0), 0);
        assert_eq!(duration_milliseconds(1), 0);
    }

    #[test]
    fn peaks_are_64_relative_rms_values() {
        let mut pcm = tone(6_400, 100);
        pcm.extend(tone(6_400, 10_000));
        let values = peaks(&pcm);
        assert_eq!(values.len(), PEAK_COUNT);
        assert_eq!(values[63], 100, "the loudest window is 100");
        assert_eq!(values[0], 1, "a quiet window is small");
        assert!(values.iter().all(|value| (0..=100).contains(value)));
        assert_eq!(peaks(&[]), vec![0; 64]);
        assert_eq!(peaks(&tone(10, 0)), vec![0; 64], "silence is all zeros");
        assert_eq!(peaks(&tone(3, 500)).len(), 64);
    }
}
