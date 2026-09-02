use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::process::Command;
use tokio::try_join;
use tracing::debug;

pub struct EncodedAudio {
    pub data: Bytes,
    pub content_type: &'static str,
}

/// Encodes raw PCM audio (mono, 16 kHz, f32 samples) into FLAC using ffmpeg.
///
/// FLAC offers lossless compression with ~40-60% smaller payloads compared to WAV
/// for 16 kHz speech, while preserving Whisper-grade accuracy. Alternative lossy
/// codecs (e.g. Opus) offer smaller payloads but cause hallucinations in tests with
/// both Groq Whisper and Gemini 2.5 Pro Flash, so we stick with FLAC here.
pub async fn encode_to_flac(audio: &[f32]) -> Result<EncodedAudio> {
    encode_with_ffmpeg(audio, "flac", "audio/flac", &["-compression_level", "12"]).await
}

/// Encodes raw PCM audio (mono, 16 kHz, f32 samples) into WAV.
///
/// whisper.cpp's server accepts WAV uploads by default. Custom OpenAI-compatible
/// endpoints use this unless configured otherwise so local server setups do not
/// require ffmpeg-side conversion on the server.
pub fn encode_to_wav(audio: &[f32]) -> Result<EncodedAudio> {
    use std::io::Write;

    let mut data: Vec<u8> = vec![];

    // Convert f32 samples to i16
    let samples_i16: Vec<i16> = audio
        .iter()
        .map(|&sample| (sample * 32767.0).clamp(-32768.0, 32767.0) as i16)
        .collect();

    let channels: u16 = 1;
    let sample_rate: u32 = 16000;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_size = (samples_i16.len() * 2) as u32;

    // RIFF header
    data.write_all(b"RIFF")?;
    data.write_all(&(36 + data_size).to_le_bytes())?;
    data.write_all(b"WAVE")?;

    // fmt chunk
    data.write_all(b"fmt ")?;
    data.write_all(&16u32.to_le_bytes())?; // Chunk size
    data.write_all(&1u16.to_le_bytes())?; // Audio format (PCM)
    data.write_all(&channels.to_le_bytes())?;
    data.write_all(&sample_rate.to_le_bytes())?;
    data.write_all(&byte_rate.to_le_bytes())?;
    data.write_all(&block_align.to_le_bytes())?;
    data.write_all(&bits_per_sample.to_le_bytes())?;

    // data chunk
    data.write_all(b"data")?;
    data.write_all(&data_size.to_le_bytes())?;

    // Write samples
    for sample in samples_i16 {
        data.write_all(&sample.to_le_bytes())?;
    }

    Ok(EncodedAudio {
        data: data.into(),
        content_type: "audio/wav",
    })
}

async fn encode_with_ffmpeg(
    audio: &[f32],
    format: &'static str,
    content_type: &'static str,
    extra_args: &[&str],
) -> Result<EncodedAudio> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
    if audio.is_empty() {
        return Ok(EncodedAudio {
            data: Bytes::new(),
            content_type,
        });
    }

    let mut child = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("f32le")
        .arg("-ar")
        .arg("16000")
        .arg("-ac")
        .arg("1")
        .arg("-i")
        .arg("pipe:0")
        .args(extra_args)
        .arg("-f")
        .arg(format)
        .arg("pipe:1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn ffmpeg for FLAC encoding. Ensure ffmpeg is installed")?;

    let mut stdin = child.stdin.take().context("Failed to open ffmpeg stdin")?;
    let mut stdout = child
        .stdout
        .take()
        .context("Failed to open ffmpeg stdout")?;
    let mut stderr = child
        .stderr
        .take()
        .context("Failed to open ffmpeg stderr")?;

    let audio_chunks = audio;

    let write_future = async move {
        let mut writer = BufWriter::new(&mut stdin);
        const CHUNK_SIZE: usize = 4096;
        let mut buffer = vec![0u8; CHUNK_SIZE * std::mem::size_of::<f32>()];

        for chunk in audio_chunks.chunks(CHUNK_SIZE) {
            let required = chunk.len() * std::mem::size_of::<f32>();
            if buffer.len() < required {
                buffer.resize(required, 0);
            }

            for (idx, sample) in chunk.iter().enumerate() {
                let bytes = sample.to_le_bytes();
                let offset = idx * 4;
                buffer[offset..offset + 4].copy_from_slice(&bytes);
            }

            writer
                .write_all(&buffer[..required])
                .await
                .context("Failed to stream PCM audio into ffmpeg")?;
        }

        writer
            .flush()
            .await
            .context("Failed to flush PCM audio into ffmpeg")?;
        stdin
            .shutdown()
            .await
            .context("Failed to close ffmpeg stdin")?;
        Ok::<(), anyhow::Error>(())
    };

    let read_future = async move {
        let mut encoded = Vec::new();
        stdout
            .read_to_end(&mut encoded)
            .await
            .context("Failed to read FLAC output from ffmpeg")?;
        Ok::<Bytes, anyhow::Error>(Bytes::from(encoded))
    };

    let stderr_future = async move {
        let mut buf = Vec::new();
        stderr
            .read_to_end(&mut buf)
            .await
            .context("Failed to read ffmpeg stderr")?;
        Ok::<Bytes, anyhow::Error>(Bytes::from(buf))
    };

    let (_, encoded, stderr_bytes) = try_join!(write_future, read_future, stderr_future)?;

    let status = child.wait().await.context("Failed to wait for ffmpeg")?;

    if !status.success() {
        let stderr_text = String::from_utf8_lossy(&stderr_bytes);
        return Err(anyhow::anyhow!(
            "ffmpeg exited with status {:?}: {}",
            status.code(),
            stderr_text
        ));
    }

    debug!(
        "Encoded PCM into {} ({} bytes -> {} bytes)",
        format,
        audio.len() * std::mem::size_of::<f32>(),
        encoded.len()
    );

    Ok(EncodedAudio {
        data: encoded,
        content_type,
    })
}
