use jni::objects::{JByteArray, JClass, JValue};
use jni::sys::{jboolean, jint, jlong};
use jni::JNIEnv;

use tslib_audio::codec::{Decoder, Encoder};

use crate::error::{audio_to_jni_result, throw_tslib_exception};

/// Internal handle that owns both encoder and decoder.
pub struct OpusHandle {
    pub encoder: tslib_audio::codec::OpusEncoder,
    pub decoder: tslib_audio::codec::OpusDecoder,
    pub config: tslib_audio::AudioConfig,
}

fn ptr_to_opus(ptr: jlong) -> &'static mut OpusHandle {
    unsafe { &mut *(ptr as *mut OpusHandle) }
}

fn opus_to_ptr(handle: OpusHandle) -> jlong {
    Box::into_raw(Box::new(handle)) as jlong
}

/// `OpusCodec()` or `OpusCodec(config)` — create with default or custom config.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeCreate(
    mut env: JNIEnv,
    _class: JClass,
    sample_rate: i32,
    channels: i32,
    bitrate: i32,
    frame_size_ms: i32,
) -> jlong {
    let mut config = tslib_audio::AudioConfig::default();
    if sample_rate > 0 {
        config.sample_rate = sample_rate as u32;
    }
    if channels > 0 {
        config.channels = channels as u16;
    }
    if bitrate > 0 {
        config.bitrate = bitrate as u32;
    }
    if frame_size_ms > 0 {
        config.frame_size_ms = frame_size_ms as u32;
    }

    let codec = match audio_to_jni_result(
        &mut env,
        tslib_audio::OpusCodec::new(config.clone()),
    ) {
        Some(c) => c,
        None => return 0,
    };

    let encoder = match audio_to_jni_result(&mut env, codec.create_encoder()) {
        Some(e) => e,
        None => return 0,
    };

    let decoder = match audio_to_jni_result(&mut env, codec.create_decoder()) {
        Some(d) => d,
        None => return 0,
    };

    opus_to_ptr(OpusHandle {
        encoder,
        decoder,
        config,
    })
}

/// `OpusCodec.nativeDestroy(ptr)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeDestroy(
    _env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) {
    if ptr != 0 {
        unsafe {
            drop(Box::from_raw(ptr as *mut OpusHandle));
        }
    }
}

/// `OpusCodec.encode(pcm)` — encode PCM (16-bit LE bytes) to Opus.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeEncode<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: jlong,
    pcm: JByteArray<'local>,
) -> jni::sys::jbyteArray {
    let pcm_bytes = match env.convert_byte_array(&pcm) {
        Ok(b) => b,
        Err(e) => {
            throw_tslib_exception(&mut env, &format!("Failed to read PCM array: {e}"));
            return std::ptr::null_mut();
        }
    };

    if pcm_bytes.len() % 2 != 0 {
        throw_tslib_exception(&mut env, "PCM data must have even length (16-bit samples)");
        return std::ptr::null_mut();
    }

    let samples: Vec<i16> = pcm_bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    let handle = ptr_to_opus(ptr);
    let mut output = vec![0u8; 4000];
    let len = match audio_to_jni_result(&mut env, handle.encoder.encode(&samples, &mut output)) {
        Some(l) => l,
        None => return std::ptr::null_mut(),
    };

    env.byte_array_from_slice(&output[..len])
        .map(|a| a.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// `OpusCodec.decode(data, fec)` — decode Opus to PCM (16-bit LE bytes).
/// With `fec` set, returns the loss-repair audio reconstructed from the FEC
/// data embedded in this packet (used to conceal the preceding lost packet).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeDecode<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: jlong,
    data: JByteArray<'local>,
    fec: jboolean,
) -> jni::sys::jbyteArray {
    let opus_bytes = match env.convert_byte_array(&data) {
        Ok(b) => b,
        Err(e) => {
            throw_tslib_exception(&mut env, &format!("Failed to read Opus array: {e}"));
            return std::ptr::null_mut();
        }
    };

    let handle = ptr_to_opus(ptr);
    let frame_samples = handle.config.frame_size_samples() * handle.config.channels as usize;
    let mut output = vec![0i16; frame_samples];

    let len = match audio_to_jni_result(
        &mut env,
        if fec != 0 {
            handle.decoder.decode_with_fec(&opus_bytes, &mut output)
        } else {
            handle.decoder.decode(&opus_bytes, &mut output)
        },
    ) {
        Some(l) => l,
        None => return std::ptr::null_mut(),
    };

    // Convert i16 samples to bytes (little-endian)
    i16_slice_to_jbyte_array(&mut env, &output[..len])
}

/// `OpusCodec.decodePlc()` — synthesize one frame of comfort audio for a
/// lost packet (packet loss concealment, no input data).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeDecodePlc<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: jlong,
) -> jni::sys::jbyteArray {
    let handle = ptr_to_opus(ptr);
    let frame_samples = handle.config.frame_size_samples() * handle.config.channels as usize;
    let mut output = vec![0i16; frame_samples];

    let len = match audio_to_jni_result(&mut env, handle.decoder.decode_plc(&mut output)) {
        Some(l) => l,
        None => return std::ptr::null_mut(),
    };

    i16_slice_to_jbyte_array(&mut env, &output[..len])
}

/// `OpusCodec.setDecoderComplexity(complexity)` — 0 to 10; see
/// `OpusDecoder::set_complexity` for what 5, 6 and 7 turn on.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeSetDecoderComplexity(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    complexity: jint,
) {
    let handle = ptr_to_opus(ptr);
    audio_to_jni_result(&mut env, handle.decoder.set_complexity(complexity));
}

/// Convert an i16 sample slice to a little-endian jbyteArray.
fn i16_slice_to_jbyte_array<'local>(
    env: &mut JNIEnv<'local>,
    samples: &[i16],
) -> jni::sys::jbyteArray {
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    env.byte_array_from_slice(&bytes)
        .map(|a| a.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// `OpusCodec.nativeGetConfig()` — returns an `AudioConfig` Java object.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeGetConfig(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jni::sys::jobject {
    let handle = ptr_to_opus(ptr);
    let config = &handle.config;

    env.new_object(
        "dev/tslib/AudioConfig",
        "(IIII)V",
        &[
            JValue::Int(config.sample_rate as i32),
            JValue::Int(config.channels as i32),
            JValue::Int(config.bitrate as i32),
            JValue::Int(config.frame_size_ms as i32),
        ],
    )
    .map(|o| o.into_raw())
    .unwrap_or(std::ptr::null_mut())
}

/// `OpusCodec.setBitrate(bitrate)` — change the encoder bitrate at runtime
/// (used by the adaptive audio loop to shed bitrate on weak networks).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeSetBitrate(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    bitrate: jint,
) {
    let handle = ptr_to_opus(ptr);
    handle.config.bitrate = bitrate.max(0) as u32;
    audio_to_jni_result(&mut env, handle.encoder.set_bitrate(bitrate.max(0) as u32));
}

/// `OpusCodec.setFec(enabled)` — toggle inband forward error correction.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeSetFec(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    enabled: jboolean,
) {
    let handle = ptr_to_opus(ptr);
    audio_to_jni_result(&mut env, handle.encoder.set_fec(enabled != 0));
}

/// `OpusCodec.setExpectedPacketLoss(percent)` — tell the encoder which loss
/// percentage to optimize its FEC redundancy for.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_OpusCodec_nativeSetExpectedPacketLoss(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    percent: jint,
) {
    let handle = ptr_to_opus(ptr);
    audio_to_jni_result(&mut env, handle.encoder.set_expected_packet_loss(percent));
}
