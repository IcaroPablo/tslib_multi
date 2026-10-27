use audiopus_sys::*;
use std::ffi::CStr;

#[test]
fn version() {
    let version = unsafe { CStr::from_ptr(opus_get_version_string()) };
    assert_eq!(version.to_str().unwrap(), "libopus 1.6.1");
}

/// A 440 Hz tone survives encoding and decoding with most of its energy.
#[test]
fn roundtrip() {
    const RATE: i32 = 48_000;
    const FRAME: usize = 960;
    unsafe {
        let mut error = 0;
        let encoder = opus_encoder_create(RATE, 1, OPUS_APPLICATION_VOIP, &mut error);
        assert_eq!(error, OPUS_OK);
        let decoder = opus_decoder_create(RATE, 1, &mut error);
        assert_eq!(error, OPUS_OK);

        let (mut energy_in, mut energy_out) = (0.0, 0.0);
        let mut packet = [0u8; 4000];
        let mut decoded = [0f32; FRAME];
        for frame in 0..50 {
            let pcm: Vec<f32> = (0..FRAME)
                .map(|i| {
                    let t = (frame * FRAME + i) as f32 / RATE as f32;
                    0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                })
                .collect();
            let len = opus_encode_float(encoder, pcm.as_ptr(), FRAME as i32, packet.as_mut_ptr(), 4000);
            assert!(len > 0, "encode failed: {len}");
            let samples =
                opus_decode_float(decoder, packet.as_ptr(), len, decoded.as_mut_ptr(), FRAME as i32, 0);
            assert_eq!(samples, FRAME as i32);
            // The first frames hold the codec's start-up delay
            if frame >= 10 {
                energy_in += pcm.iter().map(|s| s * s).sum::<f32>();
                energy_out += decoded.iter().map(|s| s * s).sum::<f32>();
            }
        }
        opus_encoder_destroy(encoder);
        opus_decoder_destroy(decoder);
        let ratio = energy_out / energy_in;
        assert!((0.8..1.2).contains(&ratio), "energy ratio {ratio}");
    }
}
