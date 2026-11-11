//! Microphone capture and WAV playback.
//!
//! The sidecar wants 16 kHz mono PCM16 little-endian. The device usually
//! does not. Mixing and resampling happen here so the socket only carries
//! the format Vosk expects. Playback is blocking and runs on a worker
//! thread; the egui frame never waits on it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const TARGET_RATE: u32 = 16_000;

/// Mix interleaved channels to mono, linearly resample to 16 kHz, and pack
/// little-endian PCM16. Samples outside [-1, 1] are clamped.
pub(crate) fn resample_to_pcm16(interleaved: &[f32], channels: u16, from_rate: u32) -> Vec<u8> {
    let mono = to_mono(interleaved, channels);
    let resampled = resample(&mono, from_rate, TARGET_RATE);
    let mut bytes = Vec::with_capacity(resampled.len() * 2);
    for sample in resampled {
        let clamped = sample.clamp(-1.0, 1.0);
        let value = (clamped * 32767.0).round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let channels = usize::from(channels.max(1));
    if channels == 1 {
        return interleaved.to_vec();
    }
    let frames = interleaved.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for frame in 0..frames {
        let start = frame * channels;
        let sum: f32 = interleaved[start..start + channels].iter().sum();
        mono.push(sum / channels as f32);
    }
    mono
}

fn resample(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if input.is_empty() || from_rate == 0 || to_rate == 0 {
        return Vec::new();
    }
    if from_rate == to_rate {
        return input.to_vec();
    }
    let out_len = (input.len() as u64 * u64::from(to_rate) / u64::from(from_rate)) as usize;
    if out_len == 0 {
        return Vec::new();
    }
    let last = input.len() - 1;
    let mut output = Vec::with_capacity(out_len);
    for index in 0..out_len {
        let position = index as f64 * f64::from(from_rate) / f64::from(to_rate);
        let left = position.floor() as usize;
        let fraction = (position - left as f64) as f32;
        let a = input[left.min(last)];
        let b = input[(left + 1).min(last)];
        output.push(a + (b - a) * fraction);
    }
    output
}

/// Kept alive for as long as the session should capture. Dropping it stops the device.
pub(crate) struct MicCapture {
    _stream: cpal::Stream,
}

pub(crate) fn start_capture(
    sender: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    muted: Arc<AtomicBool>,
) -> Result<MicCapture, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no microphone found".to_string())?;
    let supported = device
        .default_input_config()
        .map_err(|err| err.to_string())?;
    let channels = supported.channels();
    let rate = supported.sample_rate().0;
    let config: cpal::StreamConfig = supported.clone().into();

    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                push_f32(data, channels, rate, &sender, &muted);
            },
            log_stream_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let samples: Vec<f32> =
                    data.iter().map(|sample| *sample as f32 / 32768.0).collect();
                push_f32(&samples, channels, rate, &sender, &muted);
            },
            log_stream_error,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let samples: Vec<f32> = data
                    .iter()
                    .map(|sample| (*sample as f32 / 32768.0) - 1.0)
                    .collect();
                push_f32(&samples, channels, rate, &sender, &muted);
            },
            log_stream_error,
            None,
        ),
        other => return Err(format!("unsupported microphone sample format {other}")),
    }
    .map_err(|err| err.to_string())?;

    stream.play().map_err(|err| err.to_string())?;
    tracing::info!(rate, channels, "microphone open");
    Ok(MicCapture { _stream: stream })
}

fn push_f32(
    data: &[f32],
    channels: u16,
    rate: u32,
    sender: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    muted: &AtomicBool,
) {
    // While Piper is playing we drop frames. Otherwise the recognizer hears
    // the reply and treats it as the next command.
    if muted.load(Ordering::SeqCst) || data.is_empty() {
        return;
    }
    let pcm = resample_to_pcm16(data, channels, rate);
    if !pcm.is_empty() {
        let _ = sender.send(pcm);
    }
}

fn log_stream_error(err: cpal::StreamError) {
    tracing::warn!(error = %err, "microphone stream error");
}

/// Play a WAV to the default output device. Blocks until the buffer ends.
pub(crate) fn play_wav(wav: &[u8]) -> Result<(), String> {
    let (stream, handle) = rodio::OutputStream::try_default().map_err(|err| err.to_string())?;
    let sink = rodio::Sink::try_new(&handle).map_err(|err| err.to_string())?;
    let cursor = std::io::Cursor::new(wav.to_vec());
    let source = rodio::Decoder::new(cursor).map_err(|err| err.to_string())?;
    sink.append(source);
    sink.sleep_until_end();
    drop(stream);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_sample(pcm: &[u8]) -> i16 {
        i16::from_le_bytes([pcm[0], pcm[1]])
    }

    #[test]
    fn sixteen_khz_mono_round_trips_a_constant() {
        let pcm = resample_to_pcm16(&[0.5; 16], 1, 16_000);
        assert_eq!(pcm.len(), 32);
        let sample = first_sample(&pcm);
        assert!((sample - 16384).abs() < 2, "{sample}");
    }

    #[test]
    fn stereo_is_averaged_before_resample() {
        let pcm = resample_to_pcm16(&[1.0, -1.0, 1.0, -1.0], 2, 16_000);
        assert_eq!(pcm.len(), 4);
        assert_eq!(first_sample(&pcm), 0);
    }

    #[test]
    fn thirty_two_khz_halves_the_frame_count() {
        let pcm = resample_to_pcm16(&[0.25; 32], 1, 32_000);
        assert_eq!(pcm.len(), 32);
    }

    #[test]
    fn out_of_range_samples_are_clamped() {
        let pcm = resample_to_pcm16(&[2.0], 1, 16_000);
        assert_eq!(first_sample(&pcm), i16::MAX);
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(resample_to_pcm16(&[], 1, 48_000).is_empty());
    }
}
