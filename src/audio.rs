//! Audio device output (cpal), WAV import (hound) and WAV export.

use crate::engine::{AudioCmd, Engine, Meters};
use crate::project::AudioData;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc::{self, Receiver, Sender};

/// Opens the default output device and starts streaming. Returns the stream (keep it alive)
/// and the device sample rate.
pub fn start_output(
    rx: Receiver<AudioCmd>,
    pos: Arc<AtomicU64>,
    playing: Arc<AtomicBool>,
    meters: Arc<Meters>,
) -> Result<(cpal::Stream, u32), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no output device found".to_string())?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let sample_rate = supported.sample_rate();
    let format = supported.sample_format();
    let config = supported.config();
    let engine = Engine::new(sample_rate, rx, pos, playing, meters);

    let stream = match format {
        SampleFormat::F32 => build_stream::<f32>(&device, config, engine)?,
        SampleFormat::I16 => build_stream::<i16>(&device, config, engine)?,
        SampleFormat::U16 => build_stream::<u16>(&device, config, engine)?,
        SampleFormat::I32 => build_stream::<i32>(&device, config, engine)?,
        other => return Err(format!("unsupported sample format {other:?}")),
    };
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, sample_rate))
}

fn build_stream<T>(device: &cpal::Device, config: cpal::StreamConfig, mut engine: Engine) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = usize::from(config.channels.max(1));
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                scratch.resize(frames * 2, 0.0);
                engine.process(&mut scratch);
                for (frame, out) in data.chunks_mut(channels).enumerate() {
                    for (c, sample) in out.iter_mut().enumerate() {
                        let v = scratch[frame * 2 + c.min(1)];
                        *sample = T::from_sample(v.clamp(-1.0, 1.0));
                    }
                }
            },
            |err| eprintln!("BandCraft audio error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// Decodes a WAV file (int or float, any channel count) into stereo f32 samples.
pub fn load_wav(path: &str) -> Result<AudioData, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("could not open {path}: {e}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<f32>, hound::Error>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<Vec<f32>, hound::Error>>()
                .map_err(|e| e.to_string())?
        }
    };

    let frames = raw.len() / channels;
    let mut stereo = Vec::with_capacity(frames * 2);
    for f in 0..frames {
        let b = f * channels;
        let l = raw[b];
        let r = if channels > 1 { raw[b + 1] } else { l };
        stereo.push(l);
        stereo.push(r);
    }
    if stereo.is_empty() {
        return Err(format!("{path} contains no audio"));
    }
    Ok(AudioData::from_stereo(spec.sample_rate, stereo))
}

/// Writes interleaved stereo samples as a 24-bit WAV file.
pub fn write_wav(path: &str, samples: &[f32], sample_rate: u32) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
        writer.write_sample(v).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}

/// A live microphone/line-in capture. Samples arrive from the input callback and are
/// collected by `drain`; `finish` turns everything recorded so far into audio.
pub struct Recorder {
    _stream: cpal::Stream,
    rx: Receiver<Vec<f32>>,
    buffer: Vec<f32>,
    sample_rate: u32,
}

impl Recorder {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Moves captured audio from the input thread into the buffer.
    pub fn drain(&mut self) {
        while let Ok(chunk) = self.rx.try_recv() {
            self.buffer.extend_from_slice(&chunk);
        }
    }

    /// Stops capturing and returns the recording.
    pub fn finish(mut self) -> AudioData {
        self.drain();
        let samples = std::mem::take(&mut self.buffer);
        AudioData::from_stereo(self.sample_rate, samples)
    }
}

/// Opens the default input device and starts capturing (mono input is duplicated to stereo).
pub fn start_input() -> Result<Recorder, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no microphone or input device found".to_string())?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_rate = supported.sample_rate();
    let format = supported.sample_format();
    let config = supported.config();
    let (tx, rx) = mpsc::channel();
    let stream = match format {
        SampleFormat::F32 => build_input::<f32>(&device, &config, tx)?,
        SampleFormat::I16 => build_input::<i16>(&device, &config, tx)?,
        SampleFormat::U16 => build_input::<u16>(&device, &config, tx)?,
        SampleFormat::I32 => build_input::<i32>(&device, &config, tx)?,
        other => return Err(format!("unsupported input sample format {other:?}")),
    };
    stream.play().map_err(|e| e.to_string())?;
    Ok(Recorder { _stream: stream, rx, buffer: Vec::new(), sample_rate })
}

fn build_input<T>(device: &cpal::Device, config: &cpal::StreamConfig, tx: Sender<Vec<f32>>) -> Result<cpal::Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    device
        .build_input_stream(
            config.clone(),
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut out = Vec::with_capacity(data.len() / channels * 2);
                for frame in data.chunks(channels) {
                    let l = f32::from_sample_(frame[0]);
                    let r: f32 = if channels > 1 { f32::from_sample_(frame[1]) } else { l };
                    out.push(l);
                    out.push(r);
                }
                let _ = tx.send(out);
            },
            |err| eprintln!("BandCraft input error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_round_trip_keeps_length_and_rate() {
        let path = std::env::temp_dir().join("bandcraft_roundtrip_test.wav");
        let path_str = path.to_str().unwrap();
        let samples: Vec<f32> = (0..2000).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
        write_wav(path_str, &samples, 22050).unwrap();
        let data = load_wav(path_str).unwrap();
        assert_eq!(data.sample_rate, 22050);
        assert_eq!(data.samples.len(), samples.len());
        assert!(data.duration_secs() > 0.0);
        let _ = std::fs::remove_file(&path);
    }
}
