//! Small WebAssembly boundary around the pure-Rust `alac` packet decoder.
//!
//! The module intentionally does not parse an MP4/fMP4 container.  The caller
//! supplies the ALAC magic cookie once and then passes one complete ALAC audio
//! packet per call.  This makes it suitable for a fetch/worker pipeline that
//! receives encrypted fMP4 fragments from the backend and extracts samples
//! before handing PCM to an `AudioWorklet`.

use alac::{Decoder, StreamInfo};
use wasm_bindgen::prelude::*;

const F32_SCALE: f32 = 1.0 / 2_147_483_648.0;
// Apple encoders normally use 4096 samples per packet.  Keep the same
// defensive ceiling as mature ALAC decoders so an untrusted stream header
// cannot force an unbounded allocation in WebAssembly.
const MAX_FRAME_LENGTH: u32 = 4096 * 16;

fn js_error(message: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&message.to_string())
}

/// Stateful ALAC packet decoder.
///
/// A decoder must be kept for the whole track because packet decoding uses the
/// stream's codec configuration and output frame size.  ALAC packets are
/// independent at the codec level, so a caller may feed packets as soon as
/// they arrive; container parsing and network buffering stay outside this
/// type.
#[wasm_bindgen]
pub struct AlacDecoder {
    decoder: Decoder,
    /// Temporary i32 output used by `decode_packet_f32`.
    scratch: Vec<i32>,
    sample_rate: u32,
    bit_depth: u8,
    channels: u8,
    frame_length: u32,
}

#[wasm_bindgen]
impl AlacDecoder {
    /// Create a decoder from an ALAC magic cookie.
    ///
    /// The cookie is the 24-byte `alac` decoder configuration from the sample
    /// entry.  `StreamInfo::from_cookie` also accepts the optional `frma` and
    /// `alac` atom wrappers used by some MP4 writers.
    #[wasm_bindgen(constructor)]
    pub fn new(cookie: &[u8]) -> Result<AlacDecoder, JsValue> {
        let info = StreamInfo::from_cookie(cookie).map_err(js_error)?;
        let sample_rate = info.sample_rate();
        let bit_depth = info.bit_depth();
        let channels = info.channels();
        let frame_length = info.max_frames_per_packet();
        if !(1..=MAX_FRAME_LENGTH).contains(&frame_length) {
            return Err(js_error(format!(
                "ALAC frame length is outside the supported range: {frame_length}"
            )));
        }
        if !(1..=8).contains(&channels) {
            return Err(js_error(format!(
                "ALAC channel count is outside the supported range: {channels}"
            )));
        }
        if !(1..=32).contains(&bit_depth) {
            return Err(js_error(format!(
                "ALAC bit depth is outside the supported range: {bit_depth}"
            )));
        }
        if !(1..=768_000).contains(&sample_rate) {
            return Err(js_error(format!(
                "ALAC sample rate is outside the supported range: {sample_rate}"
            )));
        }
        let max_samples = info.max_samples_per_packet() as usize;

        Ok(Self {
            decoder: Decoder::new(info),
            scratch: vec![0; max_samples],
            sample_rate,
            bit_depth,
            channels,
            frame_length,
        })
    }

    /// Sample rate in Hz.
    #[wasm_bindgen(getter, js_name = sampleRate)]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Source ALAC bit depth (normally 16 or 24).
    #[wasm_bindgen(getter, js_name = bitDepth)]
    pub fn bit_depth(&self) -> u8 {
        self.bit_depth
    }

    /// Number of interleaved channels.
    #[wasm_bindgen(getter, js_name = channels)]
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Maximum decoded frames in one ALAC packet.
    #[wasm_bindgen(getter, js_name = frameLength)]
    pub fn frame_length(&self) -> u32 {
        self.frame_length
    }

    /// Maximum number of interleaved samples in one packet.
    #[wasm_bindgen(js_name = maxSamplesPerPacket)]
    pub fn max_samples_per_packet(&self) -> usize {
        self.scratch.len()
    }

    /// Decode one complete ALAC packet into interleaved signed 32-bit PCM.
    ///
    /// The decoded values are left-aligned in i32, matching the `alac` crate's
    /// `Sample for i32` contract.  The returned count is the number of values
    /// written, not the number of audio frames.  The caller should divide by
    /// `channels` when advancing its playback clock.
    #[wasm_bindgen(js_name = decodePacketI32)]
    pub fn decode_packet_i32(
        &mut self,
        packet: &[u8],
        output: &mut [i32],
    ) -> Result<usize, JsValue> {
        if output.len() < self.scratch.len() {
            return Err(js_error(format!(
                "output buffer is too small: need {}, got {}",
                self.scratch.len(),
                output.len()
            )));
        }

        let decoded = self
            .decoder
            .decode_packet(packet, &mut output[..self.scratch.len()])
            .map_err(js_error)?;
        Ok(decoded.len())
    }

    /// Decode one complete ALAC packet into interleaved normalized Float32 PCM.
    ///
    /// Values are in the range expected by Web Audio (`[-1, 1)`).  The
    /// conversion happens in Rust so an AudioWorklet only has to copy the
    /// transferred Float32Array into its ring buffer.
    #[wasm_bindgen(js_name = decodePacketF32)]
    pub fn decode_packet_f32(
        &mut self,
        packet: &[u8],
        output: &mut [f32],
    ) -> Result<usize, JsValue> {
        if output.len() < self.scratch.len() {
            return Err(js_error(format!(
                "output buffer is too small: need {}, got {}",
                self.scratch.len(),
                output.len()
            )));
        }

        let decoded = self
            .decoder
            .decode_packet(packet, &mut self.scratch)
            .map_err(js_error)?;
        for (dst, &sample) in output.iter_mut().zip(decoded.iter()) {
            *dst = sample as f32 * F32_SCALE;
        }
        Ok(decoded.len())
    }
}

#[cfg(test)]
mod tests {
    use super::AlacDecoder;

    fn cookie() -> [u8; 24] {
        let mut cookie = [0; 24];
        cookie[0..4].copy_from_slice(&256u32.to_be_bytes()); // frames per packet.
        cookie[4] = 0; // compatible version.
        cookie[5] = 24; // bit depth.
        cookie[6] = 40; // rice history multiplier.
        cookie[7] = 10; // rice initial history.
        cookie[8] = 14; // rice limit.
        cookie[9] = 2; // stereo.
        cookie[20..24].copy_from_slice(&44_100u32.to_be_bytes());
        cookie
    }

    #[test]
    fn exposes_stream_configuration() {
        let decoder = AlacDecoder::new(&cookie()).expect("valid ALAC cookie");
        assert_eq!(decoder.sample_rate(), 44_100);
        assert_eq!(decoder.bit_depth(), 24);
        assert_eq!(decoder.channels(), 2);
        assert_eq!(decoder.frame_length(), 256);
        assert_eq!(decoder.max_samples_per_packet(), 512);
    }
}
