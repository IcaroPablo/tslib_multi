//! Event system for tslib
//!
//! Events are emitted by the client when things happen on the server.

use crate::connection::ConnectionState;
use crate::state::{Channel, User};
use crate::streams::StreamInfo;
use async_trait::async_trait;

/// Events that can be emitted by the client
#[derive(Debug, Clone)]
pub enum Event {
    // Connection events
    /// Connection state changed
    ConnectionStateChanged {
        old_state: ConnectionState,
        new_state: ConnectionState,
    },
    /// Successfully connected to the server
    Connected {
        server_name: String,
        welcome_message: Option<String>,
    },
    /// Disconnected from the server
    Disconnected { reason: String },
    /// Connection lost unexpectedly
    ConnectionLost { reason: String },

    // Channel events
    /// A new channel was created
    ChannelCreated { channel: Channel },
    /// A channel was deleted
    ChannelDeleted { channel_id: u64 },
    /// A channel was edited
    ChannelEdited { channel: Channel },
    /// We moved to a different channel
    ChannelJoined { channel: Channel },

    // User events
    /// A user joined the server
    UserJoined { user: User },
    /// A user left the server
    UserLeft { user: User, reason: String },
    /// A user moved to a different channel
    UserMoved {
        user: User,
        from_channel: u64,
        to_channel: u64,
    },
    /// A user was kicked from a channel
    UserKickedFromChannel {
        user: User,
        kicker: User,
        reason: String,
    },
    /// A user was kicked from the server
    UserKickedFromServer {
        user: User,
        kicker: User,
        reason: String,
    },
    /// A user was banned
    UserBanned {
        user: User,
        banner: User,
        reason: String,
        duration: Option<u64>,
    },
    /// A user's properties changed
    UserUpdated { user: User },

    // Talk events
    /// A user started talking
    TalkStatusStart { user_id: u16, is_whisper: bool },
    /// A user stopped talking
    TalkStatusStop { user_id: u16 },

    // Message events
    /// Received a text message
    TextMessage {
        sender_id: u16,
        sender_name: String,
        message: String,
        target: MessageTarget,
    },
    /// Hidden data from another client, passed on by the server: clients of the same kind use
    /// it to talk to each other (TeleSpeak syncs chat history this way). Others ignore it.
    PluginCommand {
        name: String,
        data: String,
        invoker_id: u16,
        /// The sender's unique id, base64 like a user's
        invoker_uid: Option<String>,
    },
    /// We were poked by someone
    Poked {
        poker_id: u16,
        poker_name: String,
        message: String,
    },

    // Permission events
    /// Server group assigned to a user
    ServerGroupAssigned { user_id: u16, group_id: u64 },
    /// Server group removed from a user
    ServerGroupRemoved { user_id: u16, group_id: u64 },

    // Audio events
    /// Audio data received
    AudioReceived {
        user_id: u16,
        codec: AudioCodec,
        data: Vec<u8>,
        /// True when the packet arrived as a whisper (VoiceWhisper) packet
        is_whisper: bool,
    },

    // File transfer events
    /// A file download completed successfully
    FileDownloaded {
        channel_id: u64,
        path: String,
        data: Vec<u8>,
    },
    /// A file upload completed successfully
    FileUploaded {
        channel_id: u64,
        path: String,
    },
    /// A file transfer failed
    FileTransferFailed {
        path: String,
        error: String,
    },

    /// A server command returned an error (e.g. permission denied)
    CommandError {
        error_id: u32,
        message: String,
    },

    /// File list received for a channel directory
    FileListReceived {
        channel_id: u64,
        path: String,
        files: Vec<crate::client::FileEntry>,
    },

    /// Channel permissions updated (from permoverview query)
    ChannelPermissionsUpdated {
        channel_id: u64,
        permission_hints: u64,
    },

    // Stream events (TeamSpeak 6 screen sharing)
    /// A stream in our channel can be watched: it started, its streamer came into
    /// our channel, or we came into theirs
    StreamStarted { stream: StreamInfo },
    /// A stream in our channel ended or left our view
    StreamStopped { client_id: u16, stream_id: String },
    /// The streamer answered our join request; when accepted, with its WebRTC offer
    StreamJoinResponse {
        client_id: u16,
        stream_id: String,
        accepted: bool,
        offer: Option<String>,
    },
    /// WebRTC signalling from a streamer, as JSON
    StreamSignaling {
        client_id: u16,
        stream_id: String,
        json: String,
    },
}

/// Message target type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageTarget {
    /// Server-wide message
    Server,
    /// Channel message
    Channel,
    /// Private message
    Private,
}

/// Audio codec types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// Speex Narrowband
    SpeexNarrowband,
    /// Speex Wideband
    SpeexWideband,
    /// Speex Ultra-Wideband
    SpeexUltraWideband,
    /// CELT Mono
    CeltMono,
    /// Opus Voice
    OpusVoice,
    /// Opus Music
    OpusMusic,
}

impl AudioCodec {
    /// Get the codec ID as used in the protocol
    pub fn id(&self) -> u8 {
        match self {
            Self::SpeexNarrowband => 0,
            Self::SpeexWideband => 1,
            Self::SpeexUltraWideband => 2,
            Self::CeltMono => 3,
            Self::OpusVoice => 4,
            Self::OpusMusic => 5,
        }
    }

    /// Create from protocol codec ID
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::SpeexNarrowband),
            1 => Some(Self::SpeexWideband),
            2 => Some(Self::SpeexUltraWideband),
            3 => Some(Self::CeltMono),
            4 => Some(Self::OpusVoice),
            5 => Some(Self::OpusMusic),
            _ => None,
        }
    }

    /// Check if this is an Opus codec
    pub fn is_opus(&self) -> bool {
        matches!(self, Self::OpusVoice | Self::OpusMusic)
    }
}

/// Trait for handling events
#[async_trait]
pub trait EventHandler: Send + Sync {
    /// Called when any event occurs
    async fn on_event(&self, event: &Event) {
        // Default implementation dispatches to specific handlers
        match event {
            Event::Connected { server_name, welcome_message } => {
                self.on_connected(server_name, welcome_message.as_deref()).await;
            }
            Event::Disconnected { reason } => {
                self.on_disconnected(reason).await;
            }
            Event::TextMessage { sender_id, sender_name, message, target } => {
                self.on_text_message(*sender_id, sender_name, message, *target).await;
            }
            Event::UserJoined { user } => {
                self.on_user_joined(user).await;
            }
            Event::UserLeft { user, reason } => {
                self.on_user_left(user, reason).await;
            }
            _ => {}
        }
    }

    /// Called when connected to the server
    async fn on_connected(&self, _server_name: &str, _welcome_message: Option<&str>) {}

    /// Called when disconnected from the server
    async fn on_disconnected(&self, _reason: &str) {}

    /// Called when a text message is received
    async fn on_text_message(
        &self,
        _sender_id: u16,
        _sender_name: &str,
        _message: &str,
        _target: MessageTarget,
    ) {}

    /// Called when a user joins the server
    async fn on_user_joined(&self, _user: &User) {}

    /// Called when a user leaves the server
    async fn on_user_left(&self, _user: &User, _reason: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_id_roundtrip() {
        let codecs = [
            AudioCodec::SpeexNarrowband,
            AudioCodec::SpeexWideband,
            AudioCodec::SpeexUltraWideband,
            AudioCodec::CeltMono,
            AudioCodec::OpusVoice,
            AudioCodec::OpusMusic,
        ];
        for codec in &codecs {
            let id = codec.id();
            let restored = AudioCodec::from_id(id).unwrap();
            assert_eq!(*codec, restored);
        }
    }

    #[test]
    fn codec_ids_are_sequential() {
        assert_eq!(AudioCodec::SpeexNarrowband.id(), 0);
        assert_eq!(AudioCodec::SpeexWideband.id(), 1);
        assert_eq!(AudioCodec::SpeexUltraWideband.id(), 2);
        assert_eq!(AudioCodec::CeltMono.id(), 3);
        assert_eq!(AudioCodec::OpusVoice.id(), 4);
        assert_eq!(AudioCodec::OpusMusic.id(), 5);
    }

    #[test]
    fn invalid_codec_id_returns_none() {
        assert!(AudioCodec::from_id(6).is_none());
        assert!(AudioCodec::from_id(255).is_none());
    }

    #[test]
    fn opus_codecs_are_opus() {
        assert!(AudioCodec::OpusVoice.is_opus());
        assert!(AudioCodec::OpusMusic.is_opus());
    }

    #[test]
    fn non_opus_codecs_are_not_opus() {
        assert!(!AudioCodec::SpeexNarrowband.is_opus());
        assert!(!AudioCodec::SpeexWideband.is_opus());
        assert!(!AudioCodec::SpeexUltraWideband.is_opus());
        assert!(!AudioCodec::CeltMono.is_opus());
    }
}
