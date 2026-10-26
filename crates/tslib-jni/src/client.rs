use jni::objects::{JByteArray, JClass, JIntArray, JLongArray, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jlong, jdoubleArray, jobject, jobjectArray};
use jni::JNIEnv;

use crate::error::{throw_tslib_exception, to_jni_result};
use crate::types::{create_java_channel, create_java_event, create_java_server_info, create_java_user};
use crate::{get_string, require_string};

/// Internal handle that owns both the client and its tokio runtime.
pub struct ClientHandle {
    /// Destroy fence flag — set before the grace wait in nativeDestroy so
    /// in-flight nativeWaitEvents calls can bail out early.
    pub destroyed: std::sync::atomic::AtomicBool,
    /// Number of nativeWaitEvents calls currently blocked in native code.
    /// nativeDestroy spins until this hits zero instead of guessing a delay.
    pub in_flight_waits: std::sync::atomic::AtomicI32,
    /// Keeps the Java AudioSink object alive for as long as it is registered.
    pub audio_sink: std::sync::Mutex<Option<jni::objects::GlobalRef>>,
    pub client: tslib_core::Client,
    pub runtime: tokio::runtime::Runtime,
}

/// JavaVM handle captured on first use — needed to attach runtime worker
/// threads when invoking the audio sink callback from Rust.
static JAVA_VM: std::sync::OnceLock<jni::JavaVM> = std::sync::OnceLock::new();

fn ptr_to_handle(ptr: jlong) -> &'static mut ClientHandle {
    unsafe { &mut *(ptr as *mut ClientHandle) }
}

fn handle_to_ptr(handle: ClientHandle) -> jlong {
    Box::into_raw(Box::new(handle)) as jlong
}

/// `Client(address, identity, nickname)` — connect to a server.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeCreate(
    mut env: JNIEnv,
    _class: JClass,
    address: JString,
    identity_ptr: jlong,
    nickname: JString,
    password: JString,
    channel: JString,
) -> jlong {
    let address = match require_string(&mut env, &address) {
        Ok(s) => s,
        Err(()) => return 0,
    };
    let nickname = match require_string(&mut env, &nickname) {
        Ok(s) => s,
        Err(()) => return 0,
    };
    let password = get_string(&mut env, &password);
    let channel = get_string(&mut env, &channel);

    if identity_ptr == 0 {
        throw_tslib_exception(&mut env, "Identity pointer is null");
        return 0;
    }
    let identity = unsafe { &*(identity_ptr as *const tslib_core::Identity) };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            throw_tslib_exception(&mut env, &format!("Failed to create runtime: {e}"));
            return 0;
        }
    };

    let mut builder = tslib_core::ClientConfig::builder()
        .address(address)
        .identity(identity.clone())
        .nickname(nickname);

    if let Some(pw) = password {
        builder = builder.password(pw);
    }
    if let Some(ch) = channel {
        builder = builder.channel(ch);
    }

    let config = match to_jni_result(&mut env, builder.build()) {
        Some(c) => c,
        None => return 0,
    };

    let client = match runtime.block_on(async { tslib_core::Client::connect(config) }) {
        Ok(c) => c,
        Err(e) => {
            throw_tslib_exception(&mut env, &e.to_string());
            return 0;
        }
    };

    handle_to_ptr(ClientHandle {
        destroyed: std::sync::atomic::AtomicBool::new(false),
        in_flight_waits: std::sync::atomic::AtomicI32::new(0),
        audio_sink: std::sync::Mutex::new(None),
        client,
        runtime,
    })
}

/// `Client.nativeDestroy(ptr)` — free the client.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeDestroy(
    _env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) {
    if ptr != 0 {
        // Destroy fence: flag first, then spin until no nativeWaitEvents is
        // in flight (bounded — a stuck pump must not hang the destroyer).
        let handle = ptr_to_handle(ptr);
        handle
            .destroyed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2000);
        while handle
            .in_flight_waits
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        unsafe {
            drop(Box::from_raw(ptr as *mut ClientHandle));
        }
    }
}

/// `Client.setAudioSink(sink)` — register a low-latency audio callback.
/// `sink` may be null to unregister. The callback runs on Rust runtime
/// worker threads; it must return quickly and must not call back into
/// this client.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSetAudioSink(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    sink: JObject,
) {
    let handle = ptr_to_handle(ptr);
    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(e) => {
            throw_tslib_exception(&mut env, &format!("Failed to get JavaVM: {e}"));
            return;
        }
    };
    let _ = JAVA_VM.set(vm);

    if sink.is_null() {
        if let Ok(mut slot) = handle.audio_sink.lock() {
            *slot = None;
        }
        handle.client.set_audio_sink(None);
        return;
    }

    let global = match env.new_global_ref(&sink) {
        Ok(g) => g,
        Err(e) => {
            throw_tslib_exception(&mut env, &format!("Failed to pin audio sink: {e}"));
            return;
        }
    };
    if let Ok(mut slot) = handle.audio_sink.lock() {
        *slot = Some(global.clone());
    }

    handle.client.set_audio_sink(Some(std::sync::Arc::new(
        move |user_id: u16, seq: u16, data: &[u8], is_whisper: bool| {
            let Some(vm) = JAVA_VM.get() else { return };
            let Ok(mut env) = vm.attach_current_thread_permanently() else {
                return;
            };
            match env.byte_array_from_slice(data) {
                Ok(arr) => {
                    let _ = env.call_method(
                        &global,
                        "onAudioFrame",
                        "(II[BZ)V",
                        &[
                            JValue::Int(user_id as i32),
                            JValue::Int(seq as i32),
                            JValue::Object(&arr),
                            JValue::Bool(is_whisper as u8),
                        ],
                    );
                }
                Err(e) => log::warn!("audio sink: byte[] creation failed: {e}"),
            }
            // A throwing callback must never corrupt the pump thread
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
                log::warn!("audio sink callback threw; exception cleared");
            }
        },
    )));
}

/// `Client.waitConnected()`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeWaitConnected(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) {
    log::info!("nativeWaitConnected: waiting...");
    let handle = ptr_to_handle(ptr);
    let result = handle.runtime.block_on(handle.client.wait_connected());
    match &result {
        Ok(()) => {
            log::info!(
                "nativeWaitConnected: OK — {} users, {} channels (server_state)",
                handle.client.users().len(),
                handle.client.channels().len()
            );
            // Also check direct connection state
            let direct = handle.client.users_from_connection();
            log::info!(
                "nativeWaitConnected: direct connection has {} users",
                direct.len()
            );
        }
        Err(e) => log::warn!("nativeWaitConnected: FAILED — {}", e),
    }
    to_jni_result(&mut env, result);
}

/// `Client.processEvents()` — returns `Event[]`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeProcessEvents(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jobjectArray {
    let handle = ptr_to_handle(ptr);
    let events = match handle
        .runtime
        .block_on(handle.client.process_events())
    {
        Ok(evts) => evts,
        Err(e) => {
            throw_tslib_exception(&mut env, &e.to_string());
            return std::ptr::null_mut();
        }
    };

    events_to_java_array(&mut env, events)
}

/// `Client.waitEvents(timeoutMs)` — blocks until events arrive or the
/// timeout elapses; arrival wakes instantly (no polling interval).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeWaitEvents(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    timeout_ms: jint,
) -> jobjectArray {
    let handle = ptr_to_handle(ptr);
    if handle.destroyed.load(std::sync::atomic::Ordering::SeqCst) {
        return std::ptr::null_mut();
    }
    handle
        .in_flight_waits
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let result = handle
        .runtime
        .block_on(handle.client.wait_events(timeout_ms.max(1) as u64));
    handle
        .in_flight_waits
        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    let events = match result {
        Ok(evts) => evts,
        Err(e) => {
            throw_tslib_exception(&mut env, &e.to_string());
            return std::ptr::null_mut();
        }
    };

    events_to_java_array(&mut env, events)
}

fn events_to_java_array(env: &mut JNIEnv, events: Vec<tslib_core::Event>) -> jobjectArray {
    let event_class = match env.find_class("dev/tslib/Event") {
        Ok(c) => c,
        Err(_) => return std::ptr::null_mut(),
    };

    let array = match env.new_object_array(events.len() as i32, &event_class, &JObject::null()) {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };

    for (i, event) in events.iter().enumerate() {
        let obj = create_java_event(env, event);
        let _ = env.set_object_array_element(&array, i as i32, &obj);
    }

    array.into_raw()
}

/// `Client.disconnect()`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeDisconnect(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) {
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.disconnect());
    // Drive the tokio runtime for 500ms to ensure the disconnect packet
    // is actually sent over the network before the caller destroys us
    handle.runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    });
}

/// `Client.isConnected()`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeIsConnected(
    _env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jboolean {
    let handle = ptr_to_handle(ptr);
    handle.client.is_connected() as jboolean
}

/// `Client.getState()`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetState(
    _env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jint {
    let handle = ptr_to_handle(ptr);
    match handle.client.state() {
        tslib_core::ConnectionState::Disconnected => 0,
        tslib_core::ConnectionState::Connecting => 1,
        tslib_core::ConnectionState::Connected => 2,
        tslib_core::ConnectionState::Initializing => 3,
        tslib_core::ConnectionState::Reconnecting => 4,
    }
}

/// `Client.getClientId()` — returns `Integer` or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetClientId(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jobject {
    let handle = ptr_to_handle(ptr);
    match handle.client.client_id() {
        Some(id) => env
            .new_object(
                "java/lang/Integer",
                "(I)V",
                &[JValue::Int(id as i32)],
            )
            .map(|o| o.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// `Client.getChannelId()` — returns `Long` or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetChannelId(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jobject {
    let handle = ptr_to_handle(ptr);
    match handle.client.channel_id() {
        Some(id) => env
            .new_object(
                "java/lang/Long",
                "(J)V",
                &[JValue::Long(id as i64)],
            )
            .map(|o| o.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// `Client.getChannels()` — returns `Channel[]`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetChannels(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jobjectArray {
    let handle = ptr_to_handle(ptr);
    let channels = handle.client.channels();

    let channel_class = match env.find_class("dev/tslib/Channel") {
        Ok(c) => c,
        Err(_) => return std::ptr::null_mut(),
    };

    let array = match env.new_object_array(channels.len() as i32, &channel_class, &JObject::null())
    {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };

    for (i, ch) in channels.iter().enumerate() {
        let obj = create_java_channel(&mut env, ch);
        let _ = env.set_object_array_element(&array, i as i32, &obj);
    }

    array.into_raw()
}

/// `Client.getUsers()` — returns `User[]`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetUsers(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jobjectArray {
    let handle = ptr_to_handle(ptr);

    // Try to get users from server_state first
    let mut users = handle.client.users();

    // If server_state is empty, try syncing from tsclientlib state
    if users.is_empty() {
        log::debug!("nativeGetUsers: server_state.users is empty, trying sync_state");
        if let Err(e) = handle.client.sync_state() {
            log::warn!("nativeGetUsers: sync_state failed: {}", e);
        }
        users = handle.client.users();
    }

    // If still empty, try reading directly from tsclientlib state
    if users.is_empty() {
        log::debug!("nativeGetUsers: still empty after sync, trying direct read");
        users = handle.client.users_from_connection();
        log::info!("nativeGetUsers: direct read got {} users", users.len());
    }

    log::debug!("nativeGetUsers: returning {} users", users.len());

    let user_class = match env.find_class("dev/tslib/User") {
        Ok(c) => c,
        Err(_) => return std::ptr::null_mut(),
    };

    let array = match env.new_object_array(users.len() as i32, &user_class, &JObject::null()) {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };

    for (i, user) in users.iter().enumerate() {
        let obj = create_java_user(&mut env, user);
        let _ = env.set_object_array_element(&array, i as i32, &obj);
    }

    array.into_raw()
}

/// `Client.getChannel(id)` — returns `Channel` or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetChannel(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    id: jlong,
) -> jobject {
    let handle = ptr_to_handle(ptr);
    match handle.client.channel(id as u64) {
        Some(ch) => create_java_channel(&mut env, &ch).into_raw(),
        None => std::ptr::null_mut(),
    }
}

/// `Client.getUser(id)` — returns `User` or null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetUser(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    id: jint,
) -> jobject {
    let handle = ptr_to_handle(ptr);
    match handle.client.user(id as u16) {
        Some(u) => create_java_user(&mut env, &u).into_raw(),
        None => std::ptr::null_mut(),
    }
}

/// `Client.getServerInfo()`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetServerInfo(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jobject {
    let handle = ptr_to_handle(ptr);
    let info = handle.client.server_info();
    create_java_server_info(&mut env, &info).into_raw()
}

/// `Client.sendServerMessage(msg)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSendServerMessage(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    msg: JString,
) {
    let msg = match require_string(&mut env, &msg) {
        Ok(s) => s,
        Err(()) => return,
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.send_server_message(msg));
}

/// `Client.sendChannelMessage(msg)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSendChannelMessage(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    msg: JString,
) {
    let msg = match require_string(&mut env, &msg) {
        Ok(s) => s,
        Err(()) => return,
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.send_channel_message(msg));
}

/// `Client.sendPrivateMessage(userId, msg)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSendPrivateMessage(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    user_id: jint,
    msg: JString,
) {
    let msg = match require_string(&mut env, &msg) {
        Ok(s) => s,
        Err(()) => return,
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(
        &mut env,
        handle.client.send_private_message(user_id as u16, msg),
    );
}

/// `Client.sendPoke(userId, msg)`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSendPoke(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    user_id: jint,
    msg: JString,
) {
    let msg = match require_string(&mut env, &msg) {
        Ok(s) => s,
        Err(()) => return,
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(
        &mut env,
        handle.client.send_poke(user_id as u16, msg),
    );
}

/// `Client.moveToChannel(channelId, password)` — password may be null.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeMoveToChannel(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    channel_id: jlong,
    password: JString,
) {
    let password = get_string(&mut env, &password);
    let handle = ptr_to_handle(ptr);
    to_jni_result(
        &mut env,
        handle
            .client
            .move_to_channel_with_password(channel_id as u64, password),
    );
}

/// `Client.getNetworkStats()` — returns `double[]` of
/// `{rttMs, rttDevMs, packetLoss, packetLossIn, bytesRecvPerSec, bytesSentPerSec, lossObservedTotal}`
/// or null while not connected.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetNetworkStats(
    env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jdoubleArray {
    let handle = ptr_to_handle(ptr);
    let Some(stats) = handle.client.get_network_stats() else {
        return std::ptr::null_mut();
    };

    let values: [f64; 7] = [
        stats.rtt_ms,
        stats.rtt_dev_ms,
        stats.packet_loss as f64,
        stats.packet_loss_in as f64,
        stats.bytes_received_per_sec as f64,
        stats.bytes_sent_per_sec as f64,
        stats.loss_observed_total as f64,
    ];

    match env.new_double_array(values.len() as i32) {
        Ok(arr) => {
            if env.set_double_array_region(&arr, 0, &values).is_err() {
                return std::ptr::null_mut();
            }
            arr.into_raw()
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// `Client.updateServerVariables()` — re-request server variables so the
/// book's online counters and uptime stay current (the server only answers
/// with a `notifyserverupdated` when asked).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeUpdateServerVariables(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) {
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.send_server_variables());
}

/// `Client.syncState()`
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSyncState(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) {
    let handle = ptr_to_handle(ptr);
    let result = handle.client.sync_state();
    match &result {
        Ok(()) => {
            log::info!(
                "nativeSyncState: OK — {} users, {} channels",
                handle.client.users().len(),
                handle.client.channels().len()
            );
        }
        Err(e) => {
            log::warn!("nativeSyncState: FAILED — {}", e);
        }
    }
    to_jni_result(&mut env, result);
}

/// `Client.downloadFile(channelId, path)` — initiate a file download.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeDownloadFile(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    channel_id: jlong,
    path: JString,
) {
    let path = match require_string(&mut env, &path) {
        Ok(s) => s,
        Err(()) => return,
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(
        &mut env,
        handle.client.download_file(channel_id as u64, &path),
    );
}

/// `Client.uploadFile(channelId, path, data, overwrite)` — initiate a file upload.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeUploadFile(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    channel_id: jlong,
    path: JString,
    data: JByteArray,
    overwrite: jboolean,
) {
    let path = match require_string(&mut env, &path) {
        Ok(s) => s,
        Err(()) => return,
    };
    let bytes = match env.convert_byte_array(&data) {
        Ok(b) => b,
        Err(e) => {
            throw_tslib_exception(&mut env, &format!("Failed to read upload data: {e}"));
            return;
        }
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(
        &mut env,
        handle.client.upload_file(channel_id as u64, &path, &bytes, overwrite != 0),
    );
}

/// `Client.setInputMuted(muted)` — notify the server of our input muted state.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSetInputMuted(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    muted: jboolean,
) {
    let muted_bool = muted != 0;
    log::info!("nativeSetInputMuted: muted={}", muted_bool);
    let handle = ptr_to_handle(ptr);
    let result = handle.client.set_input_muted(muted_bool);
    match &result {
        Ok(()) => log::info!("nativeSetInputMuted: OK"),
        Err(e) => log::error!("nativeSetInputMuted: FAILED — {}", e),
    }
    to_jni_result(&mut env, result);
}

/// `Client.listFiles(channelId, path)` — request file list for a channel directory.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeListFiles(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    channel_id: jlong,
    path: JString,
) {
    let path_str = match require_string(&mut env, &path) {
        Ok(s) => s,
        Err(()) => return,
    };
    log::info!("nativeListFiles: channel={}, path={}", channel_id, path_str);
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.list_files(channel_id as u64, &path_str));
}

/// `Client.queryChannelPermissions(channelId)` — query effective permissions for current user.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeQueryChannelPermissions(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    channel_id: jlong,
) {
    log::info!("nativeQueryChannelPermissions: channel={}", channel_id);
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.query_channel_permissions(channel_id as u64));
}

/// `Client.deleteFile(channelId, name)` — delete a file on the server.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeDeleteFile(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    channel_id: jlong,
    name: JString,
) {
    let name_str = match require_string(&mut env, &name) {
        Ok(s) => s,
        Err(()) => return,
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.delete_file(channel_id as u64, &name_str));
}

/// `Client.renameFile(channelId, oldName, newName)` — rename a file on the server.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeRenameFile(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    channel_id: jlong,
    old_name: JString,
    new_name: JString,
) {
    let old = match require_string(&mut env, &old_name) {
        Ok(s) => s,
        Err(()) => return,
    };
    let new = match require_string(&mut env, &new_name) {
        Ok(s) => s,
        Err(()) => return,
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.rename_file(channel_id as u64, &old, &new));
}

/// `Client.createDirectory(channelId, dirname)` — create a directory on the server.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeCreateDirectory(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    channel_id: jlong,
    dirname: JString,
) {
    let dir = match require_string(&mut env, &dirname) {
        Ok(s) => s,
        Err(()) => return,
    };
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.create_directory(channel_id as u64, &dir));
}

/// `Client.sendAudio(data, codec)` — send encoded audio data to the server.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSendAudio(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    data: JByteArray,
    codec: jint,
) {
    let handle = ptr_to_handle(ptr);

    let audio_codec = match tslib_core::AudioCodec::from_id(codec as u8) {
        Some(c) => c,
        None => {
            throw_tslib_exception(&mut env, &format!("Invalid audio codec id: {codec}"));
            return;
        }
    };

    let bytes = match env.convert_byte_array(&data) {
        Ok(b) => b,
        Err(e) => {
            throw_tslib_exception(&mut env, &format!("Failed to read audio data: {e}"));
            return;
        }
    };

    to_jni_result(&mut env, handle.client.send_audio(&bytes, audio_codec));
}

/// Read a `long[]` into a `Vec<u64>`; a null array is an empty list.
fn read_long_array(env: &mut JNIEnv, array: &JLongArray, what: &str) -> Option<Vec<u64>> {
    if array.is_null() {
        return Some(Vec::new());
    }
    let len = match env.get_array_length(array) {
        Ok(n) => n as usize,
        Err(e) => {
            throw_tslib_exception(env, &format!("Failed to read whisper {what}: {e}"));
            return None;
        }
    };
    let mut buf = vec![0 as jlong; len];
    if let Err(e) = env.get_long_array_region(array, 0, &mut buf) {
        throw_tslib_exception(env, &format!("Failed to read whisper {what}: {e}"));
        return None;
    }
    Some(buf.into_iter().map(|v| v as u64).collect())
}

/// `Client.setWhisperTargets(clients, channels, groups)` — all arrays may be
/// empty (whisper off). Null arrays are treated as empty.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSetWhisperTargets(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    clients: JIntArray,
    channels: JLongArray,
    groups: JLongArray,
) {
    let handle = ptr_to_handle(ptr);

    let client_ids: Vec<u16> = if clients.is_null() {
        Vec::new()
    } else {
        let len = match env.get_array_length(&clients) {
            Ok(n) => n as usize,
            Err(e) => {
                throw_tslib_exception(&mut env, &format!("Failed to read whisper clients: {e}"));
                return;
            }
        };
        let mut buf = vec![0 as jint; len];
        if let Err(e) = env.get_int_array_region(&clients, 0, &mut buf) {
            throw_tslib_exception(&mut env, &format!("Failed to read whisper clients: {e}"));
            return;
        }
        buf.into_iter().map(|v| v as u16).collect()
    };

    let Some(channel_ids) = read_long_array(&mut env, &channels, "channels") else {
        return;
    };
    let Some(group_ids) = read_long_array(&mut env, &groups, "groups") else {
        return;
    };

    to_jni_result(
        &mut env,
        handle
            .client
            .set_whisper_targets(client_ids, channel_ids, group_ids),
    );
}

/// `Client.getServerGroups()` — `dev.tslib.ServerGroup[]`, sorted by id.
///
/// Empty until the server answers the `servergrouplist` request issued on
/// connect.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetServerGroups(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jobjectArray {
    let handle = ptr_to_handle(ptr);
    let groups = handle.client.list_server_groups();

    let group_class = match env.find_class("dev/tslib/ServerGroup") {
        Ok(c) => c,
        Err(_) => return std::ptr::null_mut(),
    };
    let array = match env.new_object_array(groups.len() as i32, &group_class, &JObject::null()) {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };

    for (index, (id, name)) in groups.iter().enumerate() {
        let object = crate::types::create_java_server_group(&mut env, *id, name);
        if env.set_object_array_element(&array, index as i32, &object).is_err() {
            return std::ptr::null_mut();
        }
    }

    array.into_raw()
}

/// `Client.getChannelGroups()` — `dev.tslib.ChannelGroup[]`, sorted by id.
///
/// Empty until the server answers the `channelgrouplist` request issued on
/// connect.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeGetChannelGroups(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
) -> jobjectArray {
    let handle = ptr_to_handle(ptr);
    let groups = handle.client.list_channel_groups();

    let group_class = match env.find_class("dev/tslib/ChannelGroup") {
        Ok(c) => c,
        Err(_) => return std::ptr::null_mut(),
    };
    let array = match env.new_object_array(groups.len() as i32, &group_class, &JObject::null()) {
        Ok(a) => a,
        Err(_) => return std::ptr::null_mut(),
    };

    for (index, (id, name)) in groups.iter().enumerate() {
        let object = crate::types::create_java_channel_group(&mut env, *id, name);
        if env.set_object_array_element(&array, index as i32, &object).is_err() {
            return std::ptr::null_mut();
        }
    }

    array.into_raw()
}

/// `Client.setNickname(nickname)` - temporary nickname for this session.
///
/// Sent as `clientupdate client_nickname`, which the server keeps for the
/// lifetime of the connection only; the identity file is left alone.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_tslib_Client_nativeSetNickname(
    mut env: JNIEnv,
    _class: JClass,
    ptr: jlong,
    nickname: JString,
) {
    let nickname = match require_string(&mut env, &nickname) {
        Ok(s) => s,
        Err(()) => return,
    };
    log::info!("nativeSetNickname: {}", nickname);
    let handle = ptr_to_handle(ptr);
    to_jni_result(&mut env, handle.client.set_nickname(&nickname));
}
