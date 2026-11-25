//! The default adzan sound: a gentle chime Moku generates itself. No recording is bundled (nothing
//! to license, nothing sacred played back by a toy synth): this is only a soft "it is time"
//! signal, five soft bell notes and a long, quiet chord.
//!
//! The bell is a sum of a few decaying partials (the upper ones slightly inharmonic, like a real
//! bell), each note with a short raised-cosine attack so nothing clicks. It is synthesised on
//! demand into a mono 16-bit 22.05 kHz WAV (about 350 KB), played from memory and dropped again.

use std::f64::consts::PI;

pub const SAMPLE_RATE: u32 = 22_050;
/// Total length. Everything has decayed well before; the last moments fade out linearly.
pub const SECONDS: f64 = 8.0;
/// Highest sample as a part of full scale (leaves headroom; the volume setting does the rest).
const PEAK: f64 = 0.8;
const ATTACK: f64 = 0.012;
const FADE_OUT: f64 = 0.6;

/// One struck note.
struct Note {
    start: f64,
    freq: f64,
    gain: f64,
    /// Decay time constant of the fundamental, seconds.
    decay: f64,
}

/// D major pentatonic: D5 A5 F#5 D6, then D4 + D5 + A5 together.
const PHRASE: [Note; 7] = [
    Note { start: 0.00, freq: 587.33, gain: 0.55, decay: 1.3 },
    Note { start: 0.60, freq: 880.00, gain: 0.50, decay: 1.3 },
    Note { start: 1.20, freq: 739.99, gain: 0.50, decay: 1.3 },
    Note { start: 1.80, freq: 1174.66, gain: 0.38, decay: 1.4 },
    Note { start: 2.90, freq: 293.66, gain: 0.55, decay: 1.5 },
    Note { start: 2.90, freq: 587.33, gain: 0.45, decay: 1.3 },
    Note { start: 2.90, freq: 880.00, gain: 0.32, decay: 1.2 },
];

/// (frequency ratio, amplitude, decay factor) of the partials of a bell.
const PARTIALS: [(f64, f64, f64); 4] = [(1.0, 1.0, 1.0), (2.0, 0.42, 0.6), (3.01, 0.18, 0.4), (4.17, 0.08, 0.28)];

/// The chime as mono 16-bit samples at [`SAMPLE_RATE`].
pub fn samples() -> Vec<i16> {
    let n = (SECONDS * SAMPLE_RATE as f64) as usize;
    let mut buf = vec![0f64; n];
    for note in &PHRASE {
        let first = (note.start * SAMPLE_RATE as f64) as usize;
        for (i, out) in buf.iter_mut().enumerate().skip(first) {
            let t = (i - first) as f64 / SAMPLE_RATE as f64;
            let attack = if t < ATTACK { 0.5 - 0.5 * (PI * t / ATTACK).cos() } else { 1.0 };
            let mut v = 0.0;
            for (ratio, amp, df) in PARTIALS {
                let f = note.freq * ratio;
                if f < SAMPLE_RATE as f64 / 2.0 {
                    v += amp * (-t / (note.decay * df)).exp() * (2.0 * PI * f * t).sin();
                }
            }
            *out += v * note.gain * attack;
        }
    }
    // Fade out, normalise to the peak, quantise.
    for (i, v) in buf.iter_mut().enumerate() {
        let left = (n - i) as f64 / SAMPLE_RATE as f64;
        if left < FADE_OUT {
            *v *= left / FADE_OUT;
        }
    }
    let max = buf.iter().fold(0f64, |m, v| m.max(v.abs())).max(1e-9);
    let scale = PEAK / max * i16::MAX as f64;
    buf.iter().map(|v| (v * scale).round() as i16).collect()
}

/// The chime as a WAV file in memory.
pub fn wav_bytes() -> Vec<u8> {
    let s = samples();
    let data_len = (s.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend(b"RIFF");
    v.extend((36 + data_len).to_le_bytes());
    v.extend(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes()); // PCM
    v.extend(1u16.to_le_bytes()); // mono
    v.extend(SAMPLE_RATE.to_le_bytes());
    v.extend((SAMPLE_RATE * 2).to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(16u16.to_le_bytes());
    v.extend(b"data");
    v.extend(data_len.to_le_bytes());
    for x in s {
        v.extend(x.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }
    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }

    /// Energy of `s` at `freq` (Goertzel), over `[from, from + len)` samples.
    fn power_at(s: &[i16], from: usize, len: usize, freq: f64) -> f64 {
        let w = 2.0 * PI * freq / SAMPLE_RATE as f64;
        let c = 2.0 * w.cos();
        let (mut a, mut b) = (0.0, 0.0);
        for x in &s[from..from + len] {
            let n = *x as f64 + c * a - b;
            b = a;
            a = n;
        }
        a * a + b * b - c * a * b
    }

    #[test]
    fn the_wav_is_a_valid_mono_pcm_file() {
        let w = wav_bytes();
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(&w, 4) as usize, w.len() - 8, "RIFF size");
        assert_eq!((u16_at(&w, 20), u16_at(&w, 22), u32_at(&w, 24), u16_at(&w, 34)), (1, 1, SAMPLE_RATE, 16), "PCM mono 16-bit");
        assert_eq!(u32_at(&w, 28), SAMPLE_RATE * 2, "byte rate");
        assert_eq!(&w[36..40], b"data");
        assert_eq!(u32_at(&w, 40) as usize, w.len() - 44, "data size");
        assert_eq!((w.len() - 44) / 2, (SECONDS * SAMPLE_RATE as f64) as usize);
        // The same check the island applies to a user's file accepts it.
        let dir = std::env::temp_dir().join(format!("moku-chime-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("chime.wav");
        std::fs::write(&p, &w).unwrap();
        assert_eq!(moku_core::audiofile::check(&p), Ok(()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gentle_not_clipped_and_without_clicks() {
        let s = samples();
        let peak = s.iter().map(|x| (*x as i32).abs()).max().unwrap();
        assert!((0.75..=0.81).contains(&(peak as f64 / 32767.0)), "peak {peak}");
        // Starts and ends from silence: no click.
        assert!(s[0].abs() < 30 && s[s.len() - 1].abs() < 30, "{} {}", s[0], s[s.len() - 1]);
        // The very end is quiet: the last 100 ms are well below the peak.
        let tail = &s[s.len() - SAMPLE_RATE as usize / 10..];
        let tail_peak = tail.iter().map(|x| (*x as i32).abs()).max().unwrap();
        assert!(tail_peak < peak / 20, "tail {tail_peak} vs {peak}");
        // No DC offset.
        let mean = s.iter().map(|x| *x as f64).sum::<f64>() / s.len() as f64;
        assert!(mean.abs() < 100.0, "mean {mean}");
        // Quiet overall: the average level stays well under the peak (a chime, not a siren).
        let rms = (s.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
        assert!(rms < 0.25 * 32767.0, "rms {rms}");
    }

    #[test]
    fn it_plays_the_notes_it_should() {
        let s = samples();
        // Right after the first note starts: D5 is there, an unrelated pitch is not.
        let from = (0.05 * SAMPLE_RATE as f64) as usize;
        let len = (0.25 * SAMPLE_RATE as f64) as usize;
        let d5 = power_at(&s, from, len, 587.33);
        assert!(d5 > 100.0 * power_at(&s, from, len, 1000.0), "D5 should dominate");
        assert!(d5 > 100.0 * power_at(&s, from, len, 3500.0));
        // The final chord holds D4 (293.66 Hz) which the opening does not.
        let late = (3.2 * SAMPLE_RATE as f64) as usize;
        assert!(power_at(&s, late, len, 293.66) > 20.0 * power_at(&s, from, len, 293.66));
    }

    #[test]
    fn deterministic_and_the_right_length() {
        assert_eq!(wav_bytes(), wav_bytes());
        assert!((6.0..=10.0).contains(&SECONDS));
        assert!(wav_bytes().len() < 400_000, "about 350 KB");
    }
}
