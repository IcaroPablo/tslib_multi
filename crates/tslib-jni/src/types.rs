use jni::objects::{JObject, JValue};
use jni::JNIEnv;

use tslib_core::state::{Channel, ServerInfo, User};

/// Create a Java `dev.tslib.Channel` object from a Rust `Channel`.
pub fn create_java_channel<'a>(env: &mut JNIEnv<'a>, ch: &Channel) -> JObject<'a> {
    let name = env.new_string(&ch.name).unwrap();
    let topic = match &ch.topic {
        Some(t) => JObject::from(env.new_string(t).unwrap()),
        None => JObject::null(),
    };
    let description = match &ch.description {
        Some(d) => JObject::from(env.new_string(d).unwrap()),
        None => JObject::null(),
    };

    env.new_object(
        "dev/tslib/Channel",
        "(JJLjava/lang/String;Ljava/lang/String;Ljava/lang/String;IZZZZBBIIIJJ)V",
        &[
            JValue::Long(ch.id as i64),
            JValue::Long(ch.parent_id as i64),
            JValue::Object(&JObject::from(name)),
            JValue::Object(&topic),
            JValue::Object(&description),
            JValue::Int(ch.order),
            JValue::Bool(ch.is_permanent as u8),
            JValue::Bool(ch.is_semi_permanent as u8),
            JValue::Bool(ch.is_default as u8),
            JValue::Bool(ch.has_password as u8),
            JValue::Byte(ch.codec as i8),
            JValue::Byte(ch.codec_quality as i8),
            JValue::Int(ch.max_clients),
            JValue::Int(ch.max_family_clients),
            JValue::Int(ch.needed_talk_power),
            JValue::Long(ch.icon_id),
            JValue::Long(ch.permission_hints as i64),
        ],
    )
    .unwrap_or_else(|_| JObject::null())
}

/// Create a Java `dev.tslib.User` object from a Rust `User`.
pub fn create_java_user<'a>(env: &mut JNIEnv<'a>, user: &User) -> JObject<'a> {
    let uid = env.new_string(&user.uid).unwrap();
    let nickname = env.new_string(&user.nickname).unwrap();
    let away_message = match &user.away_message {
        Some(m) => JObject::from(env.new_string(m).unwrap()),
        None => JObject::null(),
    };
    let platform = env.new_string(&user.platform).unwrap();
    let version = env.new_string(&user.version).unwrap();
    let country = match &user.country {
        Some(c) => JObject::from(env.new_string(c).unwrap()),
        None => JObject::null(),
    };
    let description = match &user.description {
        Some(d) => JObject::from(env.new_string(d).unwrap()),
        None => JObject::null(),
    };
    let avatar_id = match &user.avatar_id {
        Some(a) => JObject::from(env.new_string(a).unwrap()),
        None => JObject::null(),
    };

    // Build server_groups as long[]
    let groups = env
        .new_long_array(user.server_groups.len() as i32)
        .unwrap();
    let group_values: Vec<i64> = user.server_groups.iter().map(|&g| g as i64).collect();
    env.set_long_array_region(&groups, 0, &group_values).unwrap();

    env.new_object(
        "dev/tslib/User",
        "(ILjava/lang/String;JJLjava/lang/String;BZZZZZZZZZZILjava/lang/String;[JJLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;J)V",
        &[
            JValue::Int(user.id as i32),
            JValue::Object(&JObject::from(uid)),
            JValue::Long(user.database_id as i64),
            JValue::Long(user.channel_id as i64),
            JValue::Object(&JObject::from(nickname)),
            JValue::Byte(user.client_type as i8),
            JValue::Bool(user.is_talking as u8),
            JValue::Bool(user.is_input_muted as u8),
            JValue::Bool(user.is_output_muted as u8),
            JValue::Bool(user.has_input_hardware as u8),
            JValue::Bool(user.has_output_hardware as u8),
            JValue::Bool(user.is_away as u8),
            JValue::Bool(user.is_recording as u8),
            JValue::Bool(user.is_priority_speaker as u8),
            JValue::Bool(user.is_channel_commander as u8),
            JValue::Bool(user.is_talker as u8),
            JValue::Int(user.talk_power),
            JValue::Object(&JObject::from(away_message)),
            JValue::Object(unsafe { &JObject::from_raw(groups.into_raw()) }),
            JValue::Long(user.channel_group as i64),
            JValue::Object(&JObject::from(platform)),
            JValue::Object(&JObject::from(version)),
            JValue::Object(&country),
            JValue::Object(&description),
            JValue::Object(&avatar_id),
            JValue::Long(user.icon_id),
        ],
    )
    .unwrap_or_else(|_| JObject::null())
}

/// Create a Java `dev.tslib.ServerInfo` object from a Rust `ServerInfo`.
pub fn create_java_server_info<'a>(env: &mut JNIEnv<'a>, info: &ServerInfo) -> JObject<'a> {
    let name = env.new_string(&info.name).unwrap();
    let platform = env.new_string(&info.platform).unwrap();
    let version = env.new_string(&info.version).unwrap();
    let welcome_message = match &info.welcome_message {
        Some(m) => JObject::from(env.new_string(m).unwrap()),
        None => JObject::null(),
    };

    env.new_object(
        "dev/tslib/ServerInfo",
        "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;IIIJLjava/lang/String;J)V",
        &[
            JValue::Object(&JObject::from(name)),
            JValue::Object(&JObject::from(platform)),
            JValue::Object(&JObject::from(version)),
            JValue::Int(info.max_clients as i32),
            JValue::Int(info.clients_online as i32),
            JValue::Int(info.channels_online as i32),
            JValue::Long(info.uptime as i64),
            JValue::Object(&welcome_message),
            JValue::Long(info.icon_id),
        ],
    )
    .unwrap_or_else(|_| JObject::null())
}

/// Create a Java `dev.tslib.ServerGroup` object from a Rust `(id, name)` pair.
pub fn create_java_server_group<'a>(env: &mut JNIEnv<'a>, id: u64, name: &str) -> JObject<'a> {
    let name = env.new_string(name).unwrap();

    env.new_object(
        "dev/tslib/ServerGroup",
        "(JLjava/lang/String;)V",
        &[
            JValue::Long(id as i64),
            JValue::Object(&JObject::from(name)),
        ],
    )
    .unwrap_or_else(|_| JObject::null())
}

/// Create a Java `dev.tslib.ChannelGroup` object from a Rust `(id, name)` pair.
pub fn create_java_channel_group<'a>(env: &mut JNIEnv<'a>, id: u64, name: &str) -> JObject<'a> {
    let name = env.new_string(name).unwrap();

    env.new_object(
        "dev/tslib/ChannelGroup",
        "(JLjava/lang/String;)V",
        &[
            JValue::Long(id as i64),
            JValue::Object(&JObject::from(name)),
        ],
    )
    .unwrap_or_else(|_| JObject::null())
}

/// Create a Java `dev.tslib.Event` from a Rust `Event`.
pub fn create_java_event<'a>(
    env: &mut JNIEnv<'a>,
    event: &tslib_core::events::Event,
) -> JObject<'a> {
    use tslib_core::events::Event;

    // Create a HashMap<String, Object> for event data
    let map = env
        .new_object("java/util/HashMap", "()V", &[])
        .unwrap();

    let put_string = |env: &mut JNIEnv<'a>, map: &JObject<'a>, key: &str, val: &str| {
        let k = env.new_string(key).unwrap();
        let v = env.new_string(val).unwrap();
        let _ = env.call_method(
            map,
            "put",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            &[JValue::Object(&JObject::from(k)), JValue::Object(&JObject::from(v))],
        );
    };

    let put_int = |env: &mut JNIEnv<'a>, map: &JObject<'a>, key: &str, val: i32| {
        let k = env.new_string(key).unwrap();
        let v = env
            .new_object("java/lang/Integer", "(I)V", &[JValue::Int(val)])
            .unwrap();
        let _ = env.call_method(
            map,
            "put",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            &[JValue::Object(&JObject::from(k)), JValue::Object(&v)],
        );
    };

    let put_long = |env: &mut JNIEnv<'a>, map: &JObject<'a>, key: &str, val: i64| {
        let k = env.new_string(key).unwrap();
        let v = env
            .new_object("java/lang/Long", "(J)V", &[JValue::Long(val)])
            .unwrap();
        let _ = env.call_method(
            map,
            "put",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            &[JValue::Object(&JObject::from(k)), JValue::Object(&v)],
        );
    };

    let put_boolean = |env: &mut JNIEnv<'a>, map: &JObject<'a>, key: &str, val: bool| {
        let k = env.new_string(key).unwrap();
        let v = env
            .new_object("java/lang/Boolean", "(Z)V", &[JValue::Bool(val as u8)])
            .unwrap();
        let _ = env.call_method(
            map,
            "put",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            &[JValue::Object(&JObject::from(k)), JValue::Object(&v)],
        );
    };

    let event_type = match event {
        Event::Connected { server_name, welcome_message } => {
            put_string(env, &map, "server_name", server_name);
            if let Some(msg) = welcome_message {
                put_string(env, &map, "welcome_message", msg);
            }
            "connected"
        }
        Event::Disconnected { reason } => {
            put_string(env, &map, "reason", reason);
            "disconnected"
        }
        Event::ConnectionLost { reason } => {
            put_string(env, &map, "reason", reason);
            "connection_lost"
        }
        Event::ConnectionStateChanged { old_state, new_state } => {
            put_int(env, &map, "old_state", connection_state_to_int(*old_state));
            put_int(env, &map, "new_state", connection_state_to_int(*new_state));
            "connection_state_changed"
        }
        Event::ChannelCreated { channel } => {
            put_long(env, &map, "channel_id", channel.id as i64);
            put_string(env, &map, "channel_name", &channel.name);
            "channel_created"
        }
        Event::ChannelDeleted { channel_id } => {
            put_long(env, &map, "channel_id", *channel_id as i64);
            "channel_deleted"
        }
        Event::ChannelEdited { channel } => {
            put_long(env, &map, "channel_id", channel.id as i64);
            put_string(env, &map, "channel_name", &channel.name);
            "channel_edited"
        }
        Event::ChannelJoined { channel } => {
            put_long(env, &map, "channel_id", channel.id as i64);
            put_string(env, &map, "channel_name", &channel.name);
            "channel_joined"
        }
        Event::UserJoined { user } => {
            put_int(env, &map, "user_id", user.id as i32);
            put_string(env, &map, "nickname", &user.nickname);
            put_long(env, &map, "channel_id", user.channel_id as i64);
            "user_joined"
        }
        Event::UserLeft { user, reason } => {
            put_int(env, &map, "user_id", user.id as i32);
            put_string(env, &map, "nickname", &user.nickname);
            put_string(env, &map, "reason", reason);
            "user_left"
        }
        Event::UserMoved { user, from_channel, to_channel } => {
            put_int(env, &map, "user_id", user.id as i32);
            put_string(env, &map, "nickname", &user.nickname);
            put_long(env, &map, "from_channel", *from_channel as i64);
            put_long(env, &map, "to_channel", *to_channel as i64);
            "user_moved"
        }
        Event::UserKickedFromChannel { user, kicker, reason } => {
            put_int(env, &map, "user_id", user.id as i32);
            put_int(env, &map, "kicker_id", kicker.id as i32);
            put_string(env, &map, "reason", reason);
            "user_kicked_from_channel"
        }
        Event::UserKickedFromServer { user, kicker, reason } => {
            put_int(env, &map, "user_id", user.id as i32);
            put_int(env, &map, "kicker_id", kicker.id as i32);
            put_string(env, &map, "reason", reason);
            "user_kicked_from_server"
        }
        Event::UserBanned { user, banner, reason, duration } => {
            put_int(env, &map, "user_id", user.id as i32);
            put_int(env, &map, "banner_id", banner.id as i32);
            put_string(env, &map, "reason", reason);
            if let Some(dur) = duration {
                put_long(env, &map, "duration", *dur as i64);
            }
            "user_banned"
        }
        Event::UserUpdated { user } => {
            put_int(env, &map, "user_id", user.id as i32);
            put_string(env, &map, "nickname", &user.nickname);
            "user_updated"
        }
        Event::TalkStatusStart { user_id, is_whisper } => {
            put_int(env, &map, "user_id", *user_id as i32);
            put_boolean(env, &map, "is_whisper", *is_whisper);
            "talk_status_start"
        }
        Event::TalkStatusStop { user_id } => {
            put_int(env, &map, "user_id", *user_id as i32);
            "talk_status_stop"
        }
        Event::TextMessage { sender_id, sender_name, message, target } => {
            put_int(env, &map, "sender_id", *sender_id as i32);
            put_string(env, &map, "sender_name", sender_name);
            put_string(env, &map, "message", message);
            let target_str = match target {
                tslib_core::events::MessageTarget::Server => "server",
                tslib_core::events::MessageTarget::Channel => "channel",
                tslib_core::events::MessageTarget::Private => "private",
            };
            put_string(env, &map, "target", target_str);
            "text_message"
        }
        Event::PluginCommand { name, data, invoker_id, invoker_uid } => {
            put_string(env, &map, "name", name);
            put_string(env, &map, "data", data);
            put_int(env, &map, "invoker_id", *invoker_id as i32);
            if let Some(uid) = invoker_uid {
                put_string(env, &map, "invoker_uid", uid);
            }
            "plugin_command"
        }
        Event::Poked { poker_id, poker_name, message } => {
            put_int(env, &map, "poker_id", *poker_id as i32);
            put_string(env, &map, "poker_name", poker_name);
            put_string(env, &map, "message", message);
            "poked"
        }
        Event::ServerGroupAssigned { user_id, group_id } => {
            put_int(env, &map, "user_id", *user_id as i32);
            put_long(env, &map, "group_id", *group_id as i64);
            "server_group_assigned"
        }
        Event::ServerGroupRemoved { user_id, group_id } => {
            put_int(env, &map, "user_id", *user_id as i32);
            put_long(env, &map, "group_id", *group_id as i64);
            "server_group_removed"
        }
        Event::AudioReceived { user_id, codec, data, is_whisper } => {
            put_int(env, &map, "user_id", *user_id as i32);
            put_int(env, &map, "codec", codec.id() as i32);
            put_boolean(env, &map, "is_whisper", *is_whisper);
            // Put audio data as byte[]
            let byte_array = env.byte_array_from_slice(data).unwrap();
            let k = env.new_string("data").unwrap();
            let _ = env.call_method(
                &map,
                "put",
                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                &[JValue::Object(&JObject::from(k)), JValue::Object(&JObject::from(byte_array))],
            );
            "audio_received"
        }
        Event::FileDownloaded { channel_id, path, data } => {
            put_long(env, &map, "channel_id", *channel_id as i64);
            put_string(env, &map, "path", path);
            // Put file data as byte[]
            let byte_array = env.byte_array_from_slice(data).unwrap();
            let k = env.new_string("data").unwrap();
            let _ = env.call_method(
                &map,
                "put",
                "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                &[JValue::Object(&JObject::from(k)), JValue::Object(&JObject::from(byte_array))],
            );
            "file_downloaded"
        }
        Event::FileUploaded { channel_id, path } => {
            put_long(env, &map, "channel_id", *channel_id as i64);
            put_string(env, &map, "path", path);
            "file_uploaded"
        }
        Event::FileTransferFailed { path, error } => {
            put_string(env, &map, "path", path);
            put_string(env, &map, "error", error);
            "file_transfer_failed"
        }
        Event::FileListReceived { channel_id, path, files } => {
            put_long(env, &map, "channel_id", *channel_id as i64);
            put_string(env, &map, "path", path);
            // Serialize files as JSON array string
            let files_json: Vec<String> = files.iter().map(|f| {
                format!(r#"{{"name":"{}","size":{},"datetime":{},"is_file":{}}}"#,
                    f.name.replace('\\', "\\\\").replace('"', "\\\""),
                    f.size, f.datetime, f.is_file)
            }).collect();
            put_string(env, &map, "files", &format!("[{}]", files_json.join(",")));
            "file_list_received"
        }
        Event::CommandError { error_id, message } => {
            put_int(env, &map, "error_id", *error_id as i32);
            put_string(env, &map, "message", message);
            "command_error"
        }
        Event::ChannelPermissionsUpdated { channel_id, permission_hints } => {
            put_long(env, &map, "channel_id", *channel_id as i64);
            put_long(env, &map, "permission_hints", *permission_hints as i64);
            "channel_permissions_updated"
        }
        Event::StreamStarted { stream } => {
            put_int(env, &map, "client_id", stream.client_id as i32);
            put_string(env, &map, "stream_id", &stream.id);
            put_string(env, &map, "name", &stream.name);
            put_int(env, &map, "kind", stream.kind as i32);
            put_int(env, &map, "bitrate", stream.bitrate as i32);
            put_boolean(env, &map, "has_audio", stream.has_audio);
            "stream_started"
        }
        Event::StreamStopped { client_id, stream_id } => {
            put_int(env, &map, "client_id", *client_id as i32);
            put_string(env, &map, "stream_id", stream_id);
            "stream_stopped"
        }
        Event::StreamJoinResponse { client_id, stream_id, accepted, offer } => {
            put_int(env, &map, "client_id", *client_id as i32);
            put_string(env, &map, "stream_id", stream_id);
            put_boolean(env, &map, "accepted", *accepted);
            if let Some(offer) = offer {
                put_string(env, &map, "offer", offer);
            }
            "stream_join_response"
        }
        Event::StreamSignaling { client_id, stream_id, json } => {
            put_int(env, &map, "client_id", *client_id as i32);
            put_string(env, &map, "stream_id", stream_id);
            put_string(env, &map, "json", json);
            "stream_signaling"
        }
    };

    // Create the Event object
    let type_str = env.new_string(event_type).unwrap();
    env.new_object(
        "dev/tslib/Event",
        "(Ljava/lang/String;Ljava/util/Map;)V",
        &[
            JValue::Object(&JObject::from(type_str)),
            JValue::Object(&map),
        ],
    )
    .unwrap_or_else(|_| JObject::null())
}

fn connection_state_to_int(state: tslib_core::ConnectionState) -> i32 {
    match state {
        tslib_core::ConnectionState::Disconnected => 0,
        tslib_core::ConnectionState::Connecting => 1,
        tslib_core::ConnectionState::Connected => 2,
        tslib_core::ConnectionState::Initializing => 3,
        tslib_core::ConnectionState::Reconnecting => 4,
    }
}
