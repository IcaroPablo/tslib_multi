//! Audio codec handling (Opus)

use crate::config::{AudioConfig, OpusApplication};
use crate::error::{AudioError, Result};

/// Audio encoder trait
pub trait Encoder: Send {
    /// Encode PCM samples to compressed data
    fn encode(&mut self, pcm: &[i16], output: &mut [u8]) -> Result<usize>;

    /// Reset encoder state
    fn reset(&mut self) -> Result<()>;
}

/// Audio decoder trait
pub trait Decoder: Send {
    /// Decode compressed data to PCM samples
    fn decode(&mut self, data: &[u8], output: &mut [i16]) -> Result<usize>;

    /// Decode with packet loss concealment (no data available)
    fn decode_plc(&mut self, output: &mut [i16]) -> Result<usize>;

    /// Reset decoder state
    fn reset(&mut self) -> Result<()>;
}

/// Opus codec implementation
pub struct OpusCodec {
    config: AudioConfig,
}

impl OpusCodec {
    /// Create a new Opus codec
    pub fn new(config: AudioConfig) -> Result<Self> {
        Ok(Self { config })
    }

    /// Create an encoder
    pub fn create_encoder(&self) -> Result<OpusEncoder> {
        OpusEncoder::new(&self.config)
    }

    /// Create a decoder
    pub fn create_decoder(&self) -> Result<OpusDecoder> {
        OpusDecoder::new(&self.config)
    }
}

/// Convert OpusApplication to opus crate Application
fn opus_application(app: OpusApplication) -> opus::Application {
    match app {
        OpusApplication::Voip => opus::Application::Voip,
        OpusApplication::Audio => opus::Application::Audio,
        OpusApplication::LowDelay => opus::Application::LowDelay,
    }
}

/// Convert channel count to opus Channels
fn opus_channels(channels: u16) -> Result<opus::Channels> {
    match channels {
        1 => Ok(opus::Channels::Mono),
        2 => Ok(opus::Channels::Stereo),
        _ => Err(AudioError::InvalidConfig(format!(
            "Invalid channel count: {}. Must be 1 (mono) or 2 (stereo)",
            channels
        ))),
    }
}

/// Opus encoder
pub struct OpusEncoder {
    inner: opus::Encoder,
    channels: u16,
    frame_size: usize,
}

impl OpusEncoder {
    /// Create a new Opus encoder
    pub fn new(config: &AudioConfig) -> Result<Self> {
        let channels = opus_channels(config.channels)?;
        let application = opus_application(config.opus_application);

        let mut encoder = opus::Encoder::new(config.sample_rate, channels, application)
            .map_err(|e| AudioError::Codec(format!("Failed to create Opus encoder: {}", e)))?;

        // Set bitrate
        encoder
            .set_bitrate(opus::Bitrate::Bits(config.bitrate as i32))
            .map_err(|e| AudioError::Codec(format!("Failed to set bitrate: {}", e)))?;

        // Enable inband FEC for better packet loss handling
        encoder
            .set_inband_fec(true)
            .map_err(|e| AudioError::Codec(format!("Failed to enable FEC: {}", e)))?;

        // Set packet loss percentage expectation (helps encoder optimize)
        encoder
            .set_packet_loss_perc(5)
            .map_err(|e| AudioError::Codec(format!("Failed to set packet loss: {}", e)))?;

        // Encoder effort 0-10 (libopus default 5 is sized for the weakest
        // conceivable device). 8 buys a visible quality tier at the same
        // bitrate — including better FEC redundancy frames — for a few
        // extra MHz on frames that only get encoded while transmitting.
        encoder
            .set_complexity(8)
            .map_err(|e| AudioError::Codec(format!("Failed to set complexity: {}", e)))?;

        Ok(Self {
            inner: encoder,
            channels: config.channels,
            frame_size: config.frame_size_samples(),
        })
    }

    /// Set the bitrate
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        self.inner
            .set_bitrate(opus::Bitrate::Bits(bitrate as i32))
            .map_err(|e| AudioError::Codec(format!("Failed to set bitrate: {}", e)))?;
        Ok(())
    }

    /// Enable or disable inband forward error correction.
    ///
    /// FEC costs extra bitrate but lets the decoder reconstruct frames lost
    /// in transit — worth enabling once real packet loss is observed.
    pub fn set_fec(&mut self, enabled: bool) -> Result<()> {
        self.inner
            .set_inband_fec(enabled)
            .map_err(|e| AudioError::Codec(format!("Failed to set FEC: {}", e)))?;
        Ok(())
    }

    /// Set the expected packet loss percentage (0–100) the encoder optimizes for.
    pub fn set_expected_packet_loss(&mut self, percent: i32) -> Result<()> {
        self.inner
            .set_packet_loss_perc(percent.clamp(0, 100))
            .map_err(|e| AudioError::Codec(format!("Failed to set packet loss: {}", e)))?;
        Ok(())
    }

    /// Get expected packet size (estimate based on bitrate)
    pub fn packet_size(&self) -> usize {
        // Opus typically produces 20-200 bytes per 20ms frame at voice bitrates
        // This is just an estimate for buffer allocation
        256
    }
}

impl Encoder for OpusEncoder {
    fn encode(&mut self, pcm: &[i16], output: &mut [u8]) -> Result<usize> {
        let expected_samples = self.frame_size * self.channels as usize;
        if pcm.len() < expected_samples {
            return Err(AudioError::Encode(format!(
                "Not enough samples: got {}, expected {}",
                pcm.len(),
                expected_samples
            )));
        }

        let len = self
            .inner
            .encode(&pcm[..expected_samples], output)
            .map_err(|e| AudioError::Encode(format!("Opus encode error: {}", e)))?;

        Ok(len)
    }

    fn reset(&mut self) -> Result<()> {
        self.inner
            .reset_state()
            .map_err(|e| AudioError::Codec(format!("Failed to reset encoder: {}", e)))?;
        Ok(())
    }
}

/// Opus decoder
pub struct OpusDecoder {
    inner: opus::Decoder,
    channels: u16,
    #[allow(dead_code)]
    frame_size: usize,
}

impl OpusDecoder {
    /// Create a new Opus decoder
    pub fn new(config: &AudioConfig) -> Result<Self> {
        let channels = opus_channels(config.channels)?;

        let decoder = opus::Decoder::new(config.sample_rate, channels)
            .map_err(|e| AudioError::Codec(format!("Failed to create Opus decoder: {}", e)))?;

        Ok(Self {
            inner: decoder,
            channels: config.channels,
            frame_size: config.frame_size_samples(),
        })
    }

    /// Decode a packet with FEC decoding enabled. When the preceding packet
    /// was lost, this returns the loss-repair audio reconstructed from the
    /// FEC data embedded in THIS packet (instead of this packet's own audio)
    /// — that is how a single missing frame is recovered from its successor.
    pub fn decode_with_fec(&mut self, data: &[u8], output: &mut [i16]) -> Result<usize> {
        let samples = self
            .inner
            .decode(data, output, true)
            .map_err(|e| AudioError::Decode(format!("Opus FEC decode error: {}", e)))?;

        // samples is per channel, return total samples written
        Ok(samples * self.channels as usize)
    }
}

impl Decoder for OpusDecoder {
    fn decode(&mut self, data: &[u8], output: &mut [i16]) -> Result<usize> {
        let samples = self
            .inner
            .decode(data, output, false)
            .map_err(|e| AudioError::Decode(format!("Opus decode error: {}", e)))?;

        // samples is per channel, return total samples written
        Ok(samples * self.channels as usize)
    }

    fn decode_plc(&mut self, output: &mut [i16]) -> Result<usize> {
        // Packet loss concealment: pass None as data
        // The frame_size parameter tells opus how many samples to generate
        let samples = self
            .inner
            .decode(&[], output, true) // fec=true enables forward error correction
            .map_err(|e| AudioError::Decode(format!("Opus PLC error: {}", e)))?;

        Ok(samples * self.channels as usize)
    }

    fn reset(&mut self) -> Result<()> {
        self.inner
            .reset_state()
            .map_err(|e| AudioError::Codec(format!("Failed to reset decoder: {}", e)))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_creation() {
        let config = AudioConfig::default();
        let encoder = OpusEncoder::new(&config).unwrap();
        assert_eq!(encoder.channels, 1);
        assert_eq!(encoder.frame_size, config.frame_size_samples());
    }

    #[test]
    fn encoder_stereo() {
        let mut config = AudioConfig::default();
        config.channels = 2;
        let encoder = OpusEncoder::new(&config).unwrap();
        assert_eq!(encoder.channels, 2);
    }

    #[test]
    fn invalid_channel_count() {
        let mut config = AudioConfig::default();
        config.channels = 3;
        assert!(OpusEncoder::new(&config).is_err());
    }

    #[test]
    fn opus_channels_mapping() {
        assert!(matches!(opus_channels(1).unwrap(), opus::Channels::Mono));
        assert!(matches!(opus_channels(2).unwrap(), opus::Channels::Stereo));
        assert!(opus_channels(0).is_err());
        assert!(opus_channels(5).is_err());
    }

    #[test]
    fn opus_application_mapping() {
        assert!(matches!(opus_application(OpusApplication::Voip), opus::Application::Voip));
        assert!(matches!(opus_application(OpusApplication::Audio), opus::Application::Audio));
        assert!(matches!(opus_application(OpusApplication::LowDelay), opus::Application::LowDelay));
    }

    #[test]
    fn encode_decode_roundtrip() {
        let config = AudioConfig::default();
        let codec = OpusCodec::new(config.clone()).unwrap();
        let mut encoder = codec.create_encoder().unwrap();
        let mut decoder = codec.create_decoder().unwrap();

        let frame_size = config.frame_size_samples();
        let pcm_in: Vec<i16> = (0..frame_size as i16).collect();
        let mut encoded = vec![0u8; 4000];
        let len = encoder.encode(&pcm_in, &mut encoded).unwrap();
        assert!(len > 0);

        let mut pcm_out = vec![0i16; frame_size];
        let decoded_len = decoder.decode(&encoded[..len], &mut pcm_out).unwrap();
        assert_eq!(decoded_len, frame_size);
    }

    #[test]
    fn packet_size_estimate() {
        let config = AudioConfig::default();
        let encoder = OpusEncoder::new(&config).unwrap();
        assert_eq!(encoder.packet_size(), 256);
    }

    #[test]
    fn encoder_reset() {
        let config = AudioConfig::default();
        let mut encoder = OpusEncoder::new(&config).unwrap();
        assert!(encoder.reset().is_ok());
    }

    #[test]
    fn decoder_reset() {
        let config = AudioConfig::default();
        let codec = OpusCodec::new(config).unwrap();
        let mut decoder = codec.create_decoder().unwrap();
        assert!(decoder.reset().is_ok());
    }
}
