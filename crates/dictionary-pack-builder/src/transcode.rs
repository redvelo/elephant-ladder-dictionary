use std::io::{self, Cursor};

use ogg::reading::PacketReader;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus::{Application, Bitrate, Channels};
use rubato::{FftFixedInOut, Resampler};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use thiserror::Error;

const OPUS_RATE: usize = 48_000;
const FRAME_SAMPLES: usize = 960;
const BITRATE: i32 = 24_000;
const MAX_PACKET_BYTES: usize = 4_000;
const RESAMPLER_CHUNK_FRAMES: usize = 1_024;
const VENDOR: &[u8] = b"elephant-ladder-dictionary";

#[derive(Debug, Error)]
pub enum TranscodeError {
    #[error("cannot decode source audio: {0}")]
    Decode(String),
    #[error("cannot encode Opus audio: {0}")]
    Encode(String),
    #[error("source audio exceeds {0} seconds")]
    TooLong(u64),
}

/// A recording encoded with the format-v1 audio profile.
#[derive(Debug)]
pub struct Transcoded {
    pub bytes: Vec<u8>,
    pub duration_ms: u64,
}

/// Decodes a source recording and encodes it as mono 48 kHz Ogg Opus at 24 kbps.
///
/// Output bytes depend only on the source bytes, `serial`, and the pinned encoder,
/// resampler, and decoder versions.
///
/// # Errors
///
/// Returns an error when the source cannot be decoded, is silent or empty, exceeds
/// `max_seconds`, or cannot be encoded.
pub fn transcode_to_opus(
    source: &[u8],
    serial: u32,
    max_seconds: u64,
) -> Result<Transcoded, TranscodeError> {
    let (rate, samples) = if is_ogg_opus(source) {
        (OPUS_RATE, decode_ogg_opus(source, max_seconds)?)
    } else {
        decode_with_symphonia(source, max_seconds)?
    };
    if samples.is_empty() {
        return Err(TranscodeError::Decode(
            "source contains no samples".to_owned(),
        ));
    }
    let resampled = resample(&samples, rate)?;
    encode(&resampled, serial)
}

fn is_ogg_opus(source: &[u8]) -> bool {
    source.starts_with(b"OggS")
        && source
            .get(..512)
            .unwrap_or(source)
            .windows(8)
            .any(|window| window == b"OpusHead")
}

fn decode_with_symphonia(
    source: &[u8],
    max_seconds: u64,
) -> Result<(usize, Vec<f32>), TranscodeError> {
    let stream = MediaSourceStream::new(
        Box::new(Cursor::new(repair_riff_length(source))),
        MediaSourceStreamOptions::default(),
    );
    let mut format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            stream,
            FormatOptions::default().prebuild_seek_index(false),
            MetadataOptions::default(),
        )
        .map_err(|error| decode_error(&error))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| TranscodeError::Decode("no audio track".to_owned()))?;
    let track_id = track.id;
    let Some(CodecParameters::Audio(parameters)) = track.codec_params.clone() else {
        return Err(TranscodeError::Decode(
            "audio track has no codec parameters".to_owned(),
        ));
    };
    let mut codec = symphonia::default::get_codecs()
        .make_audio_decoder(&parameters, &AudioDecoderOptions::default())
        .map_err(|error| decode_error(&error))?;
    let mut rate = None;
    let mut mono = Vec::new();
    let mut interleaved = Vec::<f32>::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(error) => return Err(decode_error(&error)),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = codec
            .decode(&packet)
            .map_err(|error| decode_error(&error))?;
        let spec_rate = decoded.spec().rate() as usize;
        let channels = decoded.spec().channels().count();
        if channels == 0 || spec_rate == 0 || *rate.get_or_insert(spec_rate) != spec_rate {
            return Err(TranscodeError::Decode(
                "unsupported or changing audio layout".to_owned(),
            ));
        }
        decoded.copy_to_vec_interleaved::<f32>(&mut interleaved);
        downmix(&interleaved, channels, &mut mono);
        check_length(mono.len(), spec_rate, max_seconds)?;
    }
    let rate = rate.ok_or_else(|| TranscodeError::Decode("no decoded audio".to_owned()))?;
    Ok((rate, mono))
}

/// Lingua Libre WAV files declare a RIFF length four bytes shorter than their chunks,
/// which strict RIFF readers reject. Extends a short declared length to the file end.
fn repair_riff_length(source: &[u8]) -> Vec<u8> {
    let mut bytes = source.to_vec();
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        let declared = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if let Ok(actual) = u32::try_from(bytes.len() - 8)
            && declared < actual
        {
            bytes[4..8].copy_from_slice(&actual.to_le_bytes());
        }
    }
    bytes
}

fn decode_ogg_opus(source: &[u8], max_seconds: u64) -> Result<Vec<f32>, TranscodeError> {
    let mut reader = PacketReader::new(Cursor::new(source));
    let head = reader
        .read_packet()
        .map_err(|error| TranscodeError::Decode(error.to_string()))?
        .ok_or_else(|| TranscodeError::Decode("missing OpusHead".to_owned()))?;
    if head.data.len() < 19 || &head.data[..8] != b"OpusHead" {
        return Err(TranscodeError::Decode("invalid OpusHead".to_owned()));
    }
    let channels = usize::from(head.data[9]);
    let pre_skip = usize::from(u16::from_le_bytes([head.data[10], head.data[11]]));
    if !(1..=2).contains(&channels) || head.data[18] != 0 {
        return Err(TranscodeError::Decode(
            "unsupported Opus channel mapping".to_owned(),
        ));
    }
    let mut decoder = opus::Decoder::new(
        48_000,
        if channels == 1 {
            Channels::Mono
        } else {
            Channels::Stereo
        },
    )
    .map_err(|error| TranscodeError::Decode(error.to_string()))?;
    let mut mono = Vec::new();
    let mut buffer = vec![0_f32; 5_760 * channels];
    let mut last_granule = 0;
    let mut first = true;
    while let Some(packet) = reader
        .read_packet()
        .map_err(|error| TranscodeError::Decode(error.to_string()))?
    {
        if first {
            first = false;
            if packet.data.starts_with(b"OpusTags") {
                continue;
            }
        }
        let frames = decoder
            .decode_float(&packet.data, &mut buffer, false)
            .map_err(|error| TranscodeError::Decode(error.to_string()))?;
        downmix(&buffer[..frames * channels], channels, &mut mono);
        last_granule = packet.absgp_page();
        check_length(mono.len(), OPUS_RATE, max_seconds)?;
    }
    let end = usize::try_from(last_granule)
        .unwrap_or(usize::MAX)
        .saturating_sub(pre_skip)
        .min(mono.len().saturating_sub(pre_skip));
    Ok(mono.into_iter().skip(pre_skip).take(end).collect())
}

fn downmix(interleaved: &[f32], channels: usize, mono: &mut Vec<f32>) {
    #[allow(clippy::cast_precision_loss)]
    let scale = 1.0 / channels as f32;
    mono.extend(
        interleaved
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() * scale),
    );
}

fn check_length(samples: usize, rate: usize, max_seconds: u64) -> Result<(), TranscodeError> {
    if samples as u64 > max_seconds * rate as u64 {
        Err(TranscodeError::TooLong(max_seconds))
    } else {
        Ok(())
    }
}

fn resample(samples: &[f32], rate: usize) -> Result<Vec<f32>, TranscodeError> {
    if rate == OPUS_RATE {
        return Ok(samples.to_vec());
    }
    let mut resampler = FftFixedInOut::<f32>::new(rate, OPUS_RATE, RESAMPLER_CHUNK_FRAMES, 1)
        .map_err(encode_error)?;
    let delay = resampler.output_delay();
    let expected = (samples.len() * OPUS_RATE).div_ceil(rate);
    let mut output = Vec::with_capacity(expected + delay + RESAMPLER_CHUNK_FRAMES * 2);
    let mut position = 0;
    while output.len() < expected + delay {
        let needed = resampler.input_frames_next();
        let mut chunk = vec![0.0; needed];
        let available = samples.len().saturating_sub(position).min(needed);
        chunk[..available].copy_from_slice(&samples[position..position + available]);
        position += available;
        let processed = resampler.process(&[chunk], None).map_err(encode_error)?;
        output.extend_from_slice(&processed[0]);
    }
    Ok(output[delay..delay + expected].to_vec())
}

fn encode(samples: &[f32], serial: u32) -> Result<Transcoded, TranscodeError> {
    let mut encoder =
        opus::Encoder::new(48_000, Channels::Mono, Application::Voip).map_err(encode_error)?;
    encoder
        .set_bitrate(Bitrate::Bits(BITRATE))
        .map_err(encode_error)?;
    let pre_skip =
        u16::try_from(encoder.get_lookahead().map_err(encode_error)?).map_err(encode_error)?;
    let mut writer = PacketWriter::new(Vec::new());
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(1);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&48_000_u32.to_le_bytes());
    head.extend_from_slice(&0_i16.to_le_bytes());
    head.push(0);
    writer
        .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
        .map_err(encode_error)?;
    let mut tags = Vec::with_capacity(16 + VENDOR.len());
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(
        &u32::try_from(VENDOR.len())
            .map_err(encode_error)?
            .to_le_bytes(),
    );
    tags.extend_from_slice(VENDOR);
    tags.extend_from_slice(&0_u32.to_le_bytes());
    writer
        .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
        .map_err(encode_error)?;

    // Flush the encoder lookahead so the final real samples are encoded.
    let padded_len =
        (samples.len() + usize::from(pre_skip)).div_ceil(FRAME_SAMPLES) * FRAME_SAMPLES;
    let mut padded = samples.to_vec();
    padded.resize(padded_len, 0.0);
    let final_granule = u64::from(pre_skip) + samples.len() as u64;
    let mut packet = vec![0_u8; MAX_PACKET_BYTES];
    let (frames, _) = padded.as_chunks::<FRAME_SAMPLES>();
    for (index, frame) in frames.iter().enumerate() {
        let length = encoder
            .encode_float(frame, &mut packet)
            .map_err(encode_error)?;
        let last = index + 1 == frames.len();
        let granule = if last {
            final_granule
        } else {
            ((index + 1) * FRAME_SAMPLES) as u64
        };
        writer
            .write_packet(
                packet[..length].to_vec(),
                serial,
                if last {
                    PacketWriteEndInfo::EndStream
                } else {
                    PacketWriteEndInfo::NormalPacket
                },
                granule,
            )
            .map_err(encode_error)?;
    }
    Ok(Transcoded {
        bytes: writer.into_inner(),
        duration_ms: (samples.len() as u64).div_ceil(48),
    })
}

fn decode_error(error: &SymphoniaError) -> TranscodeError {
    TranscodeError::Decode(error.to_string())
}

fn encode_error(error: impl std::fmt::Display) -> TranscodeError {
    TranscodeError::Encode(error.to_string())
}
