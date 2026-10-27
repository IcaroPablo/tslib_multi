use pyo3::prelude::*;
use pyo3::types::PyDict;

/// Connection state constants.
#[pyclass(name = "ConnectionState")]
pub struct PyConnectionState;

#[pymethods]
impl PyConnectionState {
    #[classattr]
    const DISCONNECTED: u8 = 0;
    #[classattr]
    const CONNECTING: u8 = 1;
    #[classattr]
    const CONNECTED: u8 = 2;
    #[classattr]
    const INITIALIZING: u8 = 3;
    #[classattr]
    const RECONNECTING: u8 = 4;
}

/// Convert a `ConnectionState` enum to its integer representation.
pub fn connection_state_to_u8(state: tslib_core::ConnectionState) -> u8 {
    match state {
        tslib_core::ConnectionState::Disconnected => 0,
        tslib_core::ConnectionState::Connecting => 1,
        tslib_core::ConnectionState::Connected => 2,
        tslib_core::ConnectionState::Initializing => 3,
        tslib_core::ConnectionState::Reconnecting => 4,
    }
}

/// A TeamSpeak channel (read-only snapshot).
#[pyclass(name = "Channel", get_all, frozen, from_py_object)]
#[derive(Clone)]
pub struct PyChannel {
    pub id: u64,
    pub parent_id: u64,
    pub name: String,
    pub topic: Option<String>,
    pub description: Option<String>,
    pub order: i32,
    pub is_permanent: bool,
    pub is_semi_permanent: bool,
    pub is_default: bool,
    pub has_password: bool,
    pub codec: u8,
    pub codec_quality: u8,
    pub max_clients: i32,
    pub max_family_clients: i32,
    pub needed_talk_power: i32,
    pub icon_id: i64,
    pub is_subscribed: bool,
}

#[pymethods]
impl PyChannel {
    fn __repr__(&self) -> String {
        format!("Channel(id={}, name='{}')", self.id, self.name)
    }
}

impl From<tslib_core::state::Channel> for PyChannel {
    fn from(c: tslib_core::state::Channel) -> Self {
        Self {
            id: c.id,
            parent_id: c.parent_id,
            name: c.name,
            topic: c.topic,
            description: c.description,
            order: c.order,
            is_permanent: c.is_permanent,
            is_semi_permanent: c.is_semi_permanent,
            is_default: c.is_default,
            has_password: c.has_password,
            codec: c.codec,
            codec_quality: c.codec_quality,
            max_clients: c.max_clients,
            max_family_clients: c.max_family_clients,
            needed_talk_power: c.needed_talk_power,
            icon_id: c.icon_id,
            is_subscribed: c.is_subscribed,
        }
    }
}

/// A TeamSpeak user (read-only snapshot).
#[pyclass(name = "User", get_all, frozen, from_py_object)]
#[derive(Clone)]
pub struct PyUser {
    pub id: u16,
    pub uid: String,
    pub database_id: u64,
    pub channel_id: u64,
    pub nickname: String,
    pub client_type: u8,
    pub is_talking: bool,
    pub is_input_muted: bool,
    pub is_output_muted: bool,
    pub has_input_hardware: bool,
    pub has_output_hardware: bool,
    pub is_away: bool,
    pub away_message: Option<String>,
    pub is_recording: bool,
    pub is_priority_speaker: bool,
    pub is_channel_commander: bool,
    pub talk_power: i32,
    pub is_talker: bool,
    pub server_groups: Vec<u64>,
    pub channel_group: u64,
    pub platform: String,
    pub version: String,
    pub country: Option<String>,
    pub description: Option<String>,
    pub icon_id: i64,
}

#[pymethods]
impl PyUser {
    fn __repr__(&self) -> String {
        format!("User(id={}, nickname='{}')", self.id, self.nickname)
    }
}

impl From<tslib_core::state::User> for PyUser {
    fn from(u: tslib_core::state::User) -> Self {
        Self {
            id: u.id,
            uid: u.uid,
            database_id: u.database_id,
            channel_id: u.channel_id,
            nickname: u.nickname,
            client_type: u.client_type,
            is_talking: u.is_talking,
            is_input_muted: u.is_input_muted,
            is_output_muted: u.is_output_muted,
            has_input_hardware: u.has_input_hardware,
            has_output_hardware: u.has_output_hardware,
            is_away: u.is_away,
            away_message: u.away_message,
            is_recording: u.is_recording,
            is_priority_speaker: u.is_priority_speaker,
            is_channel_commander: u.is_channel_commander,
            talk_power: u.talk_power,
            is_talker: u.is_talker,
            server_groups: u.server_groups,
            channel_group: u.channel_group,
            platform: u.platform,
            version: u.version,
            country: u.country,
            description: u.description,
            icon_id: u.icon_id,
        }
    }
}

/// Server information (read-only snapshot).
#[pyclass(name = "ServerInfo", get_all, frozen, from_py_object)]
#[derive(Clone)]
pub struct PyServerInfo {
    pub name: String,
    pub platform: String,
    pub version: String,
    pub max_clients: u32,
    pub clients_online: u32,
    pub channels_online: u32,
    pub uptime: u64,
    pub welcome_message: Option<String>,
}

#[pymethods]
impl PyServerInfo {
    fn __repr__(&self) -> String {
        format!("ServerInfo(name='{}')", self.name)
    }
}

impl From<tslib_core::state::ServerInfo> for PyServerInfo {
    fn from(s: tslib_core::state::ServerInfo) -> Self {
        Self {
            name: s.name,
            platform: s.platform,
            version: s.version,
            max_clients: s.max_clients,
            clients_online: s.clients_online,
            channels_online: s.channels_online,
            uptime: s.uptime,
            welcome_message: s.welcome_message,
        }
    }
}

/// Convert a `tslib_core::events::Event` to a Python dict.
pub fn event_to_dict<'py>(py: Python<'py>, event: &tslib_core::events::Event) -> Bound<'py, PyDict> {
    use tslib_core::events::Event;

    let dict = PyDict::new(py);

    match event {
        Event::Connected { server_name, welcome_message } => {
            let _ = dict.set_item("type", "connected");
            let _ = dict.set_item("server_name", server_name);
            let _ = dict.set_item("welcome_message", welcome_message.as_deref());
        }
        Event::Disconnected { reason } => {
            let _ = dict.set_item("type", "disconnected");
            let _ = dict.set_item("reason", reason);
        }
        Event::ConnectionLost { reason } => {
            let _ = dict.set_item("type", "connection_lost");
            let _ = dict.set_item("reason", reason);
        }
        Event::ConnectionStateChanged { old_state, new_state } => {
            let _ = dict.set_item("type", "connection_state_changed");
            let _ = dict.set_item("old_state", connection_state_to_u8(*old_state));
            let _ = dict.set_item("new_state", connection_state_to_u8(*new_state));
        }
        Event::ChannelCreated { channel } => {
            let _ = dict.set_item("type", "channel_created");
            let _ = dict.set_item("channel_id", channel.id);
            let _ = dict.set_item("channel_name", &channel.name);
        }
        Event::ChannelDeleted { channel_id } => {
            let _ = dict.set_item("type", "channel_deleted");
            let _ = dict.set_item("channel_id", channel_id);
        }
        Event::ChannelEdited { channel } => {
            let _ = dict.set_item("type", "channel_edited");
            let _ = dict.set_item("channel_id", channel.id);
            let _ = dict.set_item("channel_name", &channel.name);
        }
        Event::ChannelJoined { channel } => {
            let _ = dict.set_item("type", "channel_joined");
            let _ = dict.set_item("channel_id", channel.id);
            let _ = dict.set_item("channel_name", &channel.name);
        }
        Event::UserJoined { user } => {
            let _ = dict.set_item("type", "user_joined");
            let _ = dict.set_item("user_id", user.id);
            let _ = dict.set_item("nickname", &user.nickname);
            let _ = dict.set_item("channel_id", user.channel_id);
        }
        Event::UserLeft { user, reason } => {
            let _ = dict.set_item("type", "user_left");
            let _ = dict.set_item("user_id", user.id);
            let _ = dict.set_item("nickname", &user.nickname);
            let _ = dict.set_item("reason", reason);
        }
        Event::UserMoved { user, from_channel, to_channel } => {
            let _ = dict.set_item("type", "user_moved");
            let _ = dict.set_item("user_id", user.id);
            let _ = dict.set_item("nickname", &user.nickname);
            let _ = dict.set_item("from_channel", from_channel);
            let _ = dict.set_item("to_channel", to_channel);
        }
        Event::UserKickedFromChannel { user, kicker, reason } => {
            let _ = dict.set_item("type", "user_kicked_from_channel");
            let _ = dict.set_item("user_id", user.id);
            let _ = dict.set_item("kicker_id", kicker.id);
            let _ = dict.set_item("reason", reason);
        }
        Event::UserKickedFromServer { user, kicker, reason } => {
            let _ = dict.set_item("type", "user_kicked_from_server");
            let _ = dict.set_item("user_id", user.id);
            let _ = dict.set_item("kicker_id", kicker.id);
            let _ = dict.set_item("reason", reason);
        }
        Event::UserBanned { user, banner, reason, duration } => {
            let _ = dict.set_item("type", "user_banned");
            let _ = dict.set_item("user_id", user.id);
            let _ = dict.set_item("banner_id", banner.id);
            let _ = dict.set_item("reason", reason);
            let _ = dict.set_item("duration", *duration);
        }
        Event::UserUpdated { user } => {
            let _ = dict.set_item("type", "user_updated");
            let _ = dict.set_item("user_id", user.id);
            let _ = dict.set_item("nickname", &user.nickname);
        }
        Event::TalkStatusStart { user_id, is_whisper } => {
            let _ = dict.set_item("type", "talk_status_start");
            let _ = dict.set_item("user_id", user_id);
            let _ = dict.set_item("is_whisper", is_whisper);
        }
        Event::TalkStatusStop { user_id } => {
            let _ = dict.set_item("type", "talk_status_stop");
            let _ = dict.set_item("user_id", user_id);
        }
        Event::TextMessage { sender_id, sender_name, message, target } => {
            let _ = dict.set_item("type", "text_message");
            let _ = dict.set_item("sender_id", sender_id);
            let _ = dict.set_item("sender_name", sender_name);
            let _ = dict.set_item("message", message);
            let target_str = match target {
                tslib_core::events::MessageTarget::Server => "server",
                tslib_core::events::MessageTarget::Channel => "channel",
                tslib_core::events::MessageTarget::Private => "private",
            };
            let _ = dict.set_item("target", target_str);
        }
        Event::Poked { poker_id, poker_name, message } => {
            let _ = dict.set_item("type", "poked");
            let _ = dict.set_item("poker_id", poker_id);
            let _ = dict.set_item("poker_name", poker_name);
            let _ = dict.set_item("message", message);
        }
        Event::ServerGroupAssigned { user_id, group_id } => {
            let _ = dict.set_item("type", "server_group_assigned");
            let _ = dict.set_item("user_id", user_id);
            let _ = dict.set_item("group_id", group_id);
        }
        Event::ServerGroupRemoved { user_id, group_id } => {
            let _ = dict.set_item("type", "server_group_removed");
            let _ = dict.set_item("user_id", user_id);
            let _ = dict.set_item("group_id", group_id);
        }
        Event::AudioReceived { user_id, codec, data, is_whisper } => {
            let _ = dict.set_item("type", "audio_received");
            let _ = dict.set_item("user_id", user_id);
            let _ = dict.set_item("codec", codec.id());
            let _ = dict.set_item("data_len", data.len());
            let _ = dict.set_item("is_whisper", is_whisper);
        }
        Event::FileDownloaded { channel_id, path, data } => {
            let _ = dict.set_item("type", "file_downloaded");
            let _ = dict.set_item("channel_id", channel_id);
            let _ = dict.set_item("path", path.as_str());
            let _ = dict.set_item("data_len", data.len());
        }
        Event::FileTransferFailed { path, error } => {
            let _ = dict.set_item("type", "file_transfer_failed");
            let _ = dict.set_item("path", path.as_str());
            let _ = dict.set_item("error", error.as_str());
        }
        Event::FileUploaded { channel_id, path } => {
            let _ = dict.set_item("type", "file_uploaded");
            let _ = dict.set_item("channel_id", channel_id);
            let _ = dict.set_item("path", path.as_str());
        }
        Event::CommandError { error_id, message } => {
            let _ = dict.set_item("type", "command_error");
            let _ = dict.set_item("error_id", error_id);
            let _ = dict.set_item("message", message.as_str());
        }
        Event::FileListReceived { channel_id, path, files } => {
            let _ = dict.set_item("type", "file_list_received");
            let _ = dict.set_item("channel_id", channel_id);
            let _ = dict.set_item("path", path.as_str());
            let _ = dict.set_item("file_count", files.len());
        }
        Event::ChannelPermissionsUpdated { channel_id, permission_hints } => {
            let _ = dict.set_item("type", "channel_permissions_updated");
            let _ = dict.set_item("channel_id", channel_id);
            let _ = dict.set_item("permission_hints", permission_hints);
        }
        Event::StreamStarted { stream } => {
            let _ = dict.set_item("type", "stream_started");
            let _ = dict.set_item("client_id", stream.client_id);
            let _ = dict.set_item("stream_id", stream.id.as_str());
            let _ = dict.set_item("name", stream.name.as_str());
            let _ = dict.set_item("kind", stream.kind);
            let _ = dict.set_item("bitrate", stream.bitrate);
            let _ = dict.set_item("has_audio", stream.has_audio);
        }
        Event::StreamStopped { client_id, stream_id } => {
            let _ = dict.set_item("type", "stream_stopped");
            let _ = dict.set_item("client_id", client_id);
            let _ = dict.set_item("stream_id", stream_id.as_str());
        }
        Event::StreamJoinResponse { client_id, stream_id, accepted, offer } => {
            let _ = dict.set_item("type", "stream_join_response");
            let _ = dict.set_item("client_id", client_id);
            let _ = dict.set_item("stream_id", stream_id.as_str());
            let _ = dict.set_item("accepted", accepted);
            let _ = dict.set_item("offer", offer.as_deref());
        }
        Event::StreamSignaling { client_id, stream_id, json } => {
            let _ = dict.set_item("type", "stream_signaling");
            let _ = dict.set_item("client_id", client_id);
            let _ = dict.set_item("stream_id", stream_id.as_str());
            let _ = dict.set_item("json", json.as_str());
        }
    }

    dict
}
