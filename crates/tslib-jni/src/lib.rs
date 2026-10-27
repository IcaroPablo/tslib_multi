#![allow(non_snake_case)]

mod audio;
mod bbcode;
mod client;
mod error;
mod identity;
mod types;

use jni::sys::jint;
use jni::JNIEnv;

/// Called when the native library is loaded via `System.loadLibrary`.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(_vm: jni::JavaVM, _reserved: *mut std::ffi::c_void) -> jint {
    // Initialize Android logger so Rust log macros output to logcat. Warnings and
    // errors only: the informational lines name servers, nicknames and uids, and
    // the system log is readable over USB debugging.
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Warn)
            .with_tag("tslib-jni"),
    );

    // Only `log` records reach logcat. `tracing` events (tsclientlib, and the
    // tracing macros in tslib-core) are not forwarded: nothing subscribes to
    // them, and tracing's `log` feature is off.

    log::info!("tslib-jni native library loaded");
    jni::sys::JNI_VERSION_1_6
}

/// Helper to get a `String` from a `JString`, returning `None` on null.
pub(crate) fn get_string(env: &mut JNIEnv, s: &jni::objects::JString) -> Option<String> {
    if s.is_null() {
        return None;
    }
    env.get_string(s).ok().map(|s| s.into())
}

/// Helper to get a required `String` from a `JString`.
/// Throws `TsLibException` if null.
pub(crate) fn require_string(env: &mut JNIEnv, s: &jni::objects::JString) -> Result<String, ()> {
    match get_string(env, s) {
        Some(s) => Ok(s),
        None => {
            error::throw_tslib_exception(env, "String argument must not be null");
            Err(())
        }
    }
}
