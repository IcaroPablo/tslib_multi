//! Main client implementation using tsclientlib
//!
//! The client wraps tsclientlib's Connection and provides a higher-level API.
//! Note: The client is not Send/Sync due to tsclientlib limitations.

use crate::config::ClientConfig;
use crate::connection::ConnectionState;
use crate::error::{ConnectionError, Error, Result};
use crate::events::{AudioCodec, Event, EventHandler};
use crate::state::{Channel, ServerInfo, ServerState, User};
use crate::streams::{self, StreamCommand, StreamInfo};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tsclientlib::prelude::*;
use tsclientlib::{Connection as TsConnection, DisconnectOptions, InMessage, PacketStat, Reason, StreamItem, ClientId, ChannelId};
use tsclientlib::MessageTarget as TsMessageTarget;
use tsclientlib::data::{Client as TsClient, Channel as TsChannel};
use tsclientlib::events::{Event as TsEvent, PropertyId};
use tsproto_types::{ChannelType as TsChannelType, ClientType as TsClientType, Codec as TsCodec};
use tsproto_types::errors::Error as TsError;
use tsproto_packets::packets::{AudioData, CodecType, Direction, Flags, OutAudio, OutCommand, PacketType};

/// A file entry from a channel's file list
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub datetime: i64,
    pub is_file: bool,
}

/// A snapshot of connection quality metrics from the underlying UDP layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetworkStats {
    /// Smoothed round trip time in milliseconds.
    pub rtt_ms: f64,
    /// Round trip time deviation in milliseconds.
    pub rtt_dev_ms: f64,
    /// Average incoming+outgoing packet loss as a fraction (0.0–1.0),
    /// cumulative since the connection was established.
    pub packet_loss: f32,
    /// Incoming (server→client) packet loss as a fraction (0.0–1.0).
    pub packet_loss_in: f32,
    /// Bytes received during the last second.
    pub bytes_received_per_sec: u64,
    /// Bytes sent during the last second.
    pub bytes_sent_per_sec: u64,
    /// Cumulative packets observed by the loss accounting — same denominator
    /// as `packet_loss`, so clients can derive recent-window loss by
    /// differencing (`Δlost = loss·observed − prev_loss·prev_observed`).
    pub loss_observed_total: u64,
}

/// The main TeamSpeak client
///
/// This client wraps tsclientlib's Connection. Due to tsclientlib's design,
/// the client is not Send/Sync and must be used from a single task.
///
/// Use `process_events()` to poll for and handle server events.
/// Duration after which a user is considered to have stopped talking (ms)
const TALK_TIMEOUT_MS: u128 = 300;

/// After the first stream item, wait only this long for further items so a
/// burst returns in one batch while the first frame never waits extra.
const EVENT_BURST_GRACE_MS: u64 = 3;

/// Upper bound on stream items processed per wait_events call.
const MAX_STREAM_BURST: usize = 16;

pub struct Client {
    /// The tsclientlib connection
    connection: Option<TsConnection>,
    /// Configuration
    config: ClientConfig,
    /// Current connection state
    state: ConnectionState,
    /// Server state (channels, users, etc.)
    server_state: ServerState,
    /// Event broadcaster
    event_tx: broadcast::Sender<Event>,
    /// Event handlers
    handlers: Vec<Arc<dyn EventHandler>>,
    /// Our client ID on the server
    client_id: Option<u16>,
    /// Current channel ID
    channel_id: Option<u64>,
    /// Audio packet sequence number
    audio_sequence: u16,
    /// Track which users are currently talking (user_id -> last audio timestamp)
    talking_users: HashMap<u16, Instant>,
    /// Pending file downloads: FiletransferHandle id -> (channel_id, path)
    pending_downloads: HashMap<u16, (u64, String)>,
    /// Pending file uploads: FiletransferHandle id -> (channel_id, path, data)
    pending_uploads: HashMap<u16, (u64, String, Vec<u8>)>,
    /// Local override for our own input muted state.
    /// tsclientlib's state book doesn't reflect our clientupdate commands
    /// (the server doesn't echo them back), so we track it separately.
    self_input_muted: Option<bool>,
    /// Same override for our own output (speaker) muted state.
    self_output_muted: Option<bool>,
    /// Local override for our own nickname, for the same reason: a
    /// `clientupdate client_nickname` is not echoed back to the sender, so the
    /// book keeps the old name and the member list would show it.
    self_nickname: Option<String>,
    /// Local override for our own away state (away flag, away message).
    /// The member list has to follow the user's choice even before the server
    /// answers, and TS3 does not echo our own `clientupdate` back to us.
    self_away: Option<(bool, Option<String>)>,
    /// Pending file list request: accumulates entries until FileListFinished
    pending_file_list: Option<(u64, String, Vec<FileEntry>)>,
    /// Pending permoverview request: accumulates permission entries
    /// (channel_id, HashMap<permission_id, max_value>)
    pending_perm_overview: Option<(u64, HashMap<u32, i32>)>,
    /// Active whisper targets. When non-empty, `send_audio` routes voice as
    /// `C2SWhisper` to these targets instead of the current channel (TS3
    /// semantics: whispering and channel talk are mutually exclusive).
    whisper_clients: Vec<u16>,
    whisper_channels: Vec<u64>,
    /// Server groups that should hear the whisper. A group is one target: the
    /// packet names the group and the server resolves its members, so clients
    /// that join the group later are covered without re-sending the target set.
    whisper_groups: Vec<u64>,
    /// Low-latency audio sink: when set, every received voice frame is
    /// delivered synchronously on the pump thread (sender id, sender packet
    /// sequence, payload, whisper flag) instead of being emitted
    /// as an `AudioReceived` event (which waits for the next event pull).
    audio_sink: Option<Arc<dyn Fn(u16, u16, &[u8], bool) + Send + Sync>>,
    /// Clients the server flags as streaming (`client_is_streaming`)
    streaming_clients: HashSet<u16>,
    /// Every stream the server described, by stream id (a client can have several)
    known_streams: HashMap<String, StreamInfo>,
    /// Streaming clients whose stream details were asked for
    stream_info_requested: HashSet<u16>,
    /// The streams reported to the embedder (those in our channel), by stream id
    shown_streams: HashMap<String, StreamInfo>,
}

impl Client {
    /// Connect to a TeamSpeak server
    pub fn connect(config: ClientConfig) -> Result<Self> {
        info!("Connecting to {}", config.address);

        // Create event broadcaster
        let (event_tx, _) = broadcast::channel(256);

        let mut client = Self {
            connection: None,
            config: config.clone(),
            state: ConnectionState::Disconnected,
            server_state: ServerState::default(),
            event_tx,
            handlers: Vec::new(),
            client_id: None,
            channel_id: None,
            audio_sequence: 0,
            talking_users: HashMap::new(),
            pending_downloads: HashMap::new(),
            pending_uploads: HashMap::new(),
            self_input_muted: None,
            self_output_muted: None,
            self_nickname: None,
            self_away: None,
            pending_file_list: None,
            pending_perm_overview: None,
            whisper_clients: Vec::new(),
            whisper_channels: Vec::new(),
            whisper_groups: Vec::new(),
            audio_sink: None,
            streaming_clients: HashSet::new(),
            known_streams: HashMap::new(),
            stream_info_requested: HashSet::new(),
            shown_streams: HashMap::new(),
        };

        // Establish connection
        client.do_connect()?;

        Ok(client)
    }

    /// Perform the actual connection to the server
    fn do_connect(&mut self) -> Result<()> {
        self.state = ConnectionState::Connecting;

        // Resolve the name here, with the system resolver only (Android's, which
        // honours Private DNS), and hand tsclientlib an IP address. Given a name,
        // tsclientlib first looks up TeamSpeak SRV and TSDNS records, and on
        // Android it sends those queries to Cloudflare (1.1.1.1) because it cannot
        // read the system DNS configuration; a TSDNS server would also be told the
        // address. Servers that rely on SRV or TSDNS records need their real
        // address entered instead.
        let server = {
            use std::net::ToSocketAddrs;
            self.config
                .address
                .to_socket_addrs()
                .map_err(|e| ConnectionError::ConnectFailed(e.to_string()))?
                .next()
                .ok_or_else(|| {
                    ConnectionError::ConnectFailed(format!("no address for {}", self.config.address))
                })?
        };

        // Build connection options using the new API
        // Raw commands carry the TeamSpeak 6 stream commands and flags, which
        // tsclientlib's declarations do not know (see the streams module)
        let mut options = TsConnection::build(server)
            .name(self.config.nickname.clone())
            .identity(self.config.identity.to_ts_identity())
            .raw_commands(true);

        // Add password if specified
        if let Some(ref password) = self.config.password {
            options = options.password(password.clone());
        }

        // Add channel if specified
        if let Some(ref channel) = self.config.channel {
            options = options.channel(channel.clone());
        }

        // Connect
        let con = options
            .connect()
            .map_err(|e| ConnectionError::ConnectFailed(e.to_string()))?;

        self.connection = Some(con);
        self.state = ConnectionState::Connected;

        info!("Connected to {}", self.config.address);

        // Emit connected event
        let _ = self.event_tx.send(Event::Connected {
            server_name: String::new(), // Will be updated from server data
            welcome_message: None,
        });

        Ok(())
    }

    /// Process pending events from the server
    ///
    /// This method must be called regularly to receive server events.
    /// Returns a vector of events that occurred.
    pub async fn process_events(&mut self) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut stream_items = Vec::new();
        let mut disconnected = false;

        // First, collect all pending stream items
        {
            let con = self
                .connection
                .as_mut()
                .ok_or(ConnectionError::NotConnected)?;

            // Process events with a short timeout
            let timeout = tokio::time::Duration::from_millis(10);

            loop {
                match tokio::time::timeout(timeout, con.events().next()).await {
                    Ok(Some(Ok(item))) => {
                        stream_items.push(item);
                    }
                    Ok(Some(Err(e))) => {
                        warn!("Event stream error: {}", e);
                        break;
                    }
                    Ok(None) => {
                        // Stream ended, connection closed
                        disconnected = true;
                        break;
                    }
                    Err(_) => {
                        // Timeout, no more events for now
                        break;
                    }
                }
            }
        }

        // Now process the collected items
        for item in stream_items {
            events.extend(self.process_stream_item(item).await);
        }
        events.extend(self.update_streams());

        if disconnected {
            self.state = ConnectionState::Disconnected;
            let event = Event::Disconnected {
                reason: "Connection closed".to_string(),
            };
            let _ = self.event_tx.send(event.clone());
            events.push(event);
        }

        // Check for talk status timeouts
        let talk_stop_events = self.check_talk_timeouts();
        events.extend(talk_stop_events);

        // Dispatch events to handlers
        for event in &events {
            self.dispatch_event(event.clone()).await;
        }

        Ok(events)
    }

    /// Block until at least one event is available or `timeout_ms` elapses.
    ///
    /// Latency-critical pump for the connection stream: the first item wakes
    /// instantly on arrival (no polling interval), further items are only
    /// coalesced for a short grace window so a burst returns in one batch.
    /// This replaces sleep-then-poll loops on the embedding side.
    pub async fn wait_events(&mut self, timeout_ms: u64) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut stream_items = Vec::new();
        let mut disconnected = false;

        {
            let con = self
                .connection
                .as_mut()
                .ok_or(ConnectionError::NotConnected)?;

            let mut timeout = tokio::time::Duration::from_millis(timeout_ms.max(1));
            loop {
                match tokio::time::timeout(timeout, con.events().next()).await {
                    Ok(Some(Ok(item))) => {
                        stream_items.push(item);
                        timeout = tokio::time::Duration::from_millis(EVENT_BURST_GRACE_MS);
                        if stream_items.len() >= MAX_STREAM_BURST {
                            break;
                        }
                    }
                    Ok(Some(Err(e))) => {
                        warn!("Event stream error: {}", e);
                        break;
                    }
                    Ok(None) => {
                        // Stream ended, connection closed
                        disconnected = true;
                        break;
                    }
                    Err(_) => break, // timed out — return what we have
                }
            }
        }

        for item in stream_items {
            events.extend(self.process_stream_item(item).await);
        }
        events.extend(self.update_streams());

        if disconnected {
            self.state = ConnectionState::Disconnected;
            let event = Event::Disconnected {
                reason: "Connection closed".to_string(),
            };
            let _ = self.event_tx.send(event.clone());
            events.push(event);
        }

        let talk_stop_events = self.check_talk_timeouts();
        events.extend(talk_stop_events);

        for event in &events {
            self.dispatch_event(event.clone()).await;
        }

        Ok(events)
    }

    /// Register the low-latency audio sink. Pass `None` to restore the
    /// default `AudioReceived` event delivery. The closure receives
    /// `(sender_id, sender_packet_seq, data, is_whisper)`; the sequence is
    /// the sender's per-packet counter from the TS voice header, which
    /// receivers use to detect lost frames.
    pub fn set_audio_sink(
        &mut self,
        sink: Option<Arc<dyn Fn(u16, u16, &[u8], bool) + Send + Sync>>,
    ) {
        self.audio_sink = sink;
    }

    /// Check for users who have stopped talking (no audio received recently)
    fn check_talk_timeouts(&mut self) -> Vec<Event> {
        let now = Instant::now();
        let mut stopped_users = Vec::new();

        // Find users who haven't sent audio recently
        self.talking_users.retain(|&user_id, last_audio| {
            let elapsed = now.duration_since(*last_audio).as_millis();
            if elapsed > TALK_TIMEOUT_MS {
                stopped_users.push(user_id);
                false // Remove from tracking
            } else {
                true // Keep tracking
            }
        });

        // Generate TalkStatusStop events
        stopped_users
            .into_iter()
            .map(|user_id| {
                debug!("User {} stopped talking", user_id);
                let event = Event::TalkStatusStop { user_id };
                let _ = self.event_tx.send(event.clone());
                event
            })
            .collect()
    }

    /// Wait for the initial connection to be established
    ///
    /// This waits until we receive the first BookEvents, indicating
    /// the connection is fully established. It also automatically
    /// subscribes to all channels and synchronizes the server state
    /// (users, channels, etc.).
    pub async fn wait_connected(&mut self) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        // Hard bound: the embedding drives this on its single native thread —
        // an unresponsive server must not pin that thread forever.
        const CONNECT_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
        let deadline = tokio::time::Instant::now() + CONNECT_WAIT_TIMEOUT;

        // Poll events one by one without filtering, so we don't
        // discard non-BookEvents items (messages, audio, etc.)
        loop {
            match tokio::time::timeout_at(deadline, con.events().next()).await {
                Ok(Some(Ok(item))) => {
                    if matches!(item, StreamItem::BookEvents(_)) {
                        break;
                    }
                    // Other events during connection setup are expected; just skip them
                }
                Ok(Some(Err(e))) => {
                    return Err(ConnectionError::ConnectFailed(e.to_string()).into());
                }
                Ok(None) => {
                    return Err(
                        ConnectionError::ConnectFailed("Connection closed".to_string()).into(),
                    );
                }
                Err(_elapsed) => {
                    return Err(ConnectionError::ConnectFailed(
                        "connect wait timed out".to_string(),
                    )
                    .into());
                }
            }
        }

        // Update connection state
        self.state = ConnectionState::Connected;

        // Subscribe to all channels so we can see all users
        self.subscribe_all_channels()?;

        // Synchronize full state from server
        self.sync_state()?;

        // Ask for the server group list so whisper-to-group targets can be
        // named in the UI. Not critical - a failure only means groups stay
        // unnamed.
        if let Err(e) = self.request_server_group_list() {
            warn!("Failed to request server group list: {}", e);
        }

        // Same for the channel group list: the member panel names the channel
        // group a client holds, and the client list only carries its id.
        if let Err(e) = self.request_channel_group_list() {
            warn!("Failed to request channel group list: {}", e);
        }

        // Ask the server to push current variable values (client/channel
        // counts, uptime) via notifyserverupdated. Not critical — a failure
        // only means those values stay at their defaults.
        if let Err(e) = self.send_server_variables() {
            warn!("Failed to request server variables: {}", e);
        }

        // Emit connected event
        let _ = self.event_tx.send(Event::Connected {
            server_name: self.server_state.server.name.clone(),
            welcome_message: self.server_state.server.welcome_message.clone(),
        });

        info!(
            "Connected to {} - {} users, {} channels",
            self.server_state.server.name,
            self.server_state.users.len(),
            self.server_state.channels.len()
        );

        // Default to input muted (PTT mode) so other clients see the mute icon
        if let Err(e) = self.set_input_muted(true) {
            warn!("Failed to set initial input muted state: {}", e);
        }

        Ok(())
    }

    /// Subscribe to all channels on the server
    ///
    /// This makes all users visible regardless of which channel they are in.
    pub fn subscribe_all_channels(&mut self) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "channelsubscribeall",
        );

        cmd.send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        debug!("Subscribed to all channels");
        Ok(())
    }

    /// Request the current server variables
    ///
    /// The server answers with a `notifyserverupdated`, which tsclientlib
    /// stores in `state.server.optional_data` (clients_online,
    /// channels_online, uptime). Without this request those values are
    /// never populated.
    pub fn send_server_variables(&mut self) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "servergetvariables",
        );

        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;

        debug!("Requested server variables");
        Ok(())
    }

    /// Process a stream item from tsclientlib
    ///
    /// Returns all events generated by this stream item (may be multiple,
    /// e.g. TalkStatusStart + AudioReceived, or multiple BookEvents).
    async fn process_stream_item(&mut self, item: StreamItem) -> Vec<Event> {
        match item {
            StreamItem::BookEvents(book_events) => {
                // Process book events (client/channel changes)
                let mut events = Vec::new();
                for event in book_events {
                    if let Some(evt) = self.process_book_event(event) {
                        // Send via broadcast channel
                        let _ = self.event_tx.send(evt.clone());
                        events.push(evt);
                    }
                }
                events
            }
            StreamItem::Audio(audio) => {
                // Audio received - extract data from InAudioBuf
                let audio_data = audio.data().data();
                let (from_id, seq, codec, data, is_whisper) = match audio_data {
                    AudioData::S2C { id, from, codec, data } => (*from, *id, *codec, data, false),
                    AudioData::S2CWhisper { id, from, codec, data } => (*from, *id, *codec, data, true),
                    _ => return Vec::new(), // C2S packets should not be received
                };

                let mut events = Vec::new();

                // Check if this is a new talk session (user wasn't talking before)
                let was_talking = self.talking_users.contains_key(&from_id);
                self.talking_users.insert(from_id, Instant::now());

                if !was_talking {
                    // Emit TalkStatusStart — both via broadcast AND in returned events
                    let start_event = Event::TalkStatusStart { user_id: from_id, is_whisper };
                    let _ = self.event_tx.send(start_event.clone());
                    events.push(start_event);
                    debug!("User {} started talking (whisper: {})", from_id, is_whisper);
                }

                // Hot path: deliver to the registered sink synchronously (no
                // event round-trip); fall back to the event stream otherwise
                if let Some(sink) = self.audio_sink.as_ref() {
                    sink(from_id, seq, data, is_whisper);
                } else {
                    let event = Event::AudioReceived {
                        user_id: from_id,
                        codec: codec_type_to_audio_codec(codec),
                        data: data.to_vec(),
                        is_whisper,
                    };
                    let _ = self.event_tx.send(event.clone());
                    events.push(event);
                }
                events
            }
            StreamItem::DisconnectedTemporarily(_reason) => {
                self.state = ConnectionState::Reconnecting;
                let event = Event::ConnectionLost {
                    reason: "Temporary disconnect".to_string(),
                };
                let _ = self.event_tx.send(event.clone());
                vec![event]
            }
            StreamItem::MessageEvent(msg) => {
                log::info!("StreamItem::MessageEvent: cmd={:?}", msg.get_command_name());
                self.process_message_event(msg).into_iter().collect()
            }
            StreamItem::FileDownload(handle, mut result) => {
                let (channel_id, path) = self
                    .pending_downloads
                    .remove(&handle.0)
                    .unwrap_or((0, String::new()));

                // Cap download size at 10MB for safety
                let size = result.size.min(10_485_760) as usize;
                info!("FileDownload received: path={}, size={}", path, size);
                let mut data = vec![0u8; size];

                // Timeout the read to avoid blocking the event loop
                let read_timeout = tokio::time::Duration::from_secs(5);
                match tokio::time::timeout(read_timeout, result.stream.read_exact(&mut data)).await {
                    Ok(Ok(_)) => {
                        info!("File downloaded: path={}, size={}", path, size);
                        let event = Event::FileDownloaded {
                            channel_id,
                            path,
                            data,
                        };
                        let _ = self.event_tx.send(event.clone());
                        vec![event]
                    }
                    Ok(Err(e)) => {
                        warn!("File download read failed: {}", e);
                        let event = Event::FileTransferFailed {
                            path,
                            error: e.to_string(),
                        };
                        let _ = self.event_tx.send(event.clone());
                        vec![event]
                    }
                    Err(_) => {
                        warn!("File download read timed out: path={}", path);
                        let event = Event::FileTransferFailed {
                            path,
                            error: "Read timed out".to_string(),
                        };
                        let _ = self.event_tx.send(event.clone());
                        vec![event]
                    }
                }
            }
            StreamItem::FileUpload(handle, mut result) => {
                if let Some((channel_id, path, data)) = self.pending_uploads.remove(&handle.0) {
                    let offset = result.seek_position as usize;
                    let to_write = &data[offset.min(data.len())..];
                    let write_timeout = tokio::time::Duration::from_secs(10);
                    match tokio::time::timeout(write_timeout, result.stream.write_all(to_write)).await {
                        Ok(Ok(_)) => {
                            info!("Upload complete: {} ({} bytes)", path, data.len());
                            let event = Event::FileUploaded { channel_id, path };
                            let _ = self.event_tx.send(event.clone());
                            vec![event]
                        }
                        Ok(Err(e)) => {
                            warn!("Upload write failed: {} — {}", path, e);
                            let event = Event::FileTransferFailed {
                                path,
                                error: e.to_string(),
                            };
                            let _ = self.event_tx.send(event.clone());
                            vec![event]
                        }
                        Err(_) => {
                            warn!("Upload write timed out: path={}", path);
                            let event = Event::FileTransferFailed {
                                path,
                                error: "Write timed out".to_string(),
                            };
                            let _ = self.event_tx.send(event.clone());
                            vec![event]
                        }
                    }
                } else {
                    Vec::new()
                }
            }
            StreamItem::FiletransferFailed(handle, error) => {
                // Check downloads first, then uploads
                let (_channel_id, path) = self
                    .pending_downloads
                    .remove(&handle.0)
                    .or_else(|| self.pending_uploads.remove(&handle.0).map(|(ch, p, _)| (ch, p)))
                    .unwrap_or((0, String::new()));

                warn!("File transfer failed: path={}, error={}", path, error);
                let event = Event::FileTransferFailed {
                    path,
                    error: error.to_string(),
                };
                let _ = self.event_tx.send(event.clone());
                vec![event]
            }
            StreamItem::RawCommand(raw) => self.process_raw_command(&raw),
            other => {
                debug!("Unhandled StreamItem variant: {:?}", std::mem::discriminant(&other));
                Vec::new()
            }
        }
    }

    /// Process a book event and return any high-level events to emit
    fn process_book_event(&mut self, event: TsEvent) -> Option<Event> {
        match &event {
            TsEvent::PropertyAdded { id, .. } => {
                debug!("Property added: {:?}", id);
                self.handle_property_added(id)
            }
            TsEvent::PropertyChanged { id, old, .. } => {
                debug!("Property changed: {:?}", id);
                self.handle_property_changed(id, old)
            }
            TsEvent::PropertyRemoved { id, old, .. } => {
                debug!("Property removed: {:?}", id);
                self.handle_property_removed(id, old)
            }
            TsEvent::Message { target, invoker, message } => {
                match target {
                    TsMessageTarget::Poke(_) => {
                        let event = Event::Poked {
                            poker_id: invoker.id.0,
                            poker_name: invoker.name.clone(),
                            message: message.clone(),
                        };
                        debug!("Poked by {} ({}): {}", invoker.name, invoker.id.0, message);
                        Some(event)
                    }
                    _ => {
                        let msg_target = match target {
                            TsMessageTarget::Server => crate::events::MessageTarget::Server,
                            TsMessageTarget::Channel => crate::events::MessageTarget::Channel,
                            TsMessageTarget::Client(_) => crate::events::MessageTarget::Private,
                            TsMessageTarget::Poke(_) => unreachable!(),
                        };
                        let event = Event::TextMessage {
                            sender_id: invoker.id.0,
                            sender_name: invoker.name.clone(),
                            message: message.clone(),
                            target: msg_target,
                        };
                        debug!(
                            "Text message from {} ({}): {}",
                            invoker.name, invoker.id.0, message
                        );
                        Some(event)
                    }
                }
            }
        }
    }

    /// Handle a property being added (new client, channel, etc.)
    fn handle_property_added(&mut self, id: &PropertyId) -> Option<Event> {
        let con = self.connection.as_ref()?;
        let state = con.get_state().ok()?;

        match id {
            PropertyId::Client(client_id) => {
                // New client connected
                if let Some(ts_client) = state.clients.get(client_id) {
                    let user = ts_client_to_user(ts_client);
                    self.server_state.users.insert(user.id, user.clone());
                    info!("User joined: {} ({})", user.nickname, user.id);
                    return Some(Event::UserJoined { user });
                }
            }
            PropertyId::Channel(channel_id) => {
                // New channel created
                if let Some(ts_channel) = state.channels.get(channel_id) {
                    let mut channel = ts_channel_to_channel(ts_channel);
                    // Preserve our computed permission_hints (tsclientlib doesn't track permoverview results)
                    if channel.permission_hints == 0 {
                        if let Some(existing) = self.server_state.channels.get(&channel.id) {
                            channel.permission_hints = existing.permission_hints;
                        }
                    }
                    self.server_state.channels.insert(channel.id, channel.clone());
                    info!("Channel created: {} ({})", channel.name, channel.id);
                    return Some(Event::ChannelCreated { channel });
                }
            }
            _ => {}
        }
        None
    }

    /// Handle a property being changed
    fn handle_property_changed(&mut self, id: &PropertyId, _old: &tsclientlib::events::PropertyValue) -> Option<Event> {
        let con = self.connection.as_ref()?;
        let state = con.get_state().ok()?;

        match id {
            PropertyId::ClientChannel(client_id) => {
                // Client moved to a different channel
                if let Some(ts_client) = state.clients.get(client_id) {
                    let user = ts_client_to_user(ts_client);
                    let from_channel = self.server_state.users
                        .get(&user.id)
                        .map(|u| u.channel_id)
                        .unwrap_or(0);

                    self.server_state.users.insert(user.id, user.clone());

                    if from_channel != user.channel_id {
                        info!("User {} moved from channel {} to {}", user.nickname, from_channel, user.channel_id);
                        return Some(Event::UserMoved {
                            user,
                            from_channel,
                            to_channel: ts_client.channel.0,
                        });
                    }
                }
            }
            PropertyId::ClientName(client_id) |
            PropertyId::ClientInputMuted(client_id) |
            PropertyId::ClientOutputMuted(client_id) |
            PropertyId::ClientAwayMessage(client_id) |
            PropertyId::ClientTalkPower(client_id) |
            PropertyId::ClientTalkPowerGranted(client_id) |
            PropertyId::ClientIsRecording(client_id) => {
                // Client property changed - update our state
                if let Some(ts_client) = state.clients.get(client_id) {
                    let user = ts_client_to_user(ts_client);
                    self.server_state.users.insert(user.id, user.clone());
                    return Some(Event::UserUpdated { user });
                }
            }
            PropertyId::ChannelName(channel_id) |
            PropertyId::ChannelTopic(channel_id) |
            PropertyId::ChannelCodec(channel_id) |
            PropertyId::ChannelMaxClients(channel_id) |
            PropertyId::ChannelNeededTalkPower(channel_id) => {
                // Channel property changed
                if let Some(ts_channel) = state.channels.get(channel_id) {
                    let mut channel = ts_channel_to_channel(ts_channel);
                    // Preserve our computed permission_hints
                    if channel.permission_hints == 0 {
                        if let Some(existing) = self.server_state.channels.get(&channel.id) {
                            channel.permission_hints = existing.permission_hints;
                        }
                    }
                    self.server_state.channels.insert(channel.id, channel.clone());
                    return Some(Event::ChannelEdited { channel });
                }
            }
            _ => {}
        }
        None
    }

    /// Handle a property being removed (client left, channel deleted, etc.)
    fn handle_property_removed(&mut self, id: &PropertyId, _old: &tsclientlib::events::PropertyValue) -> Option<Event> {
        match id {
            PropertyId::Client(client_id) => {
                // Client disconnected
                if let Some(user) = self.server_state.users.remove(&client_id.0) {
                    info!("User left: {} ({})", user.nickname, user.id);
                    return Some(Event::UserLeft {
                        user,
                        reason: "Disconnected".to_string(),
                    });
                }
            }
            PropertyId::Channel(channel_id) => {
                // Channel deleted
                if self.server_state.channels.remove(&channel_id.0).is_some() {
                    info!("Channel deleted: {}", channel_id.0);
                    return Some(Event::ChannelDeleted {
                        channel_id: channel_id.0,
                    });
                }
            }
            _ => {}
        }
        None
    }

    /// Synchronize the full state from tsclientlib
    /// Call this after connection to populate initial state
    pub fn sync_state(&mut self) -> Result<()> {
        let con = self.connection.as_ref().ok_or(ConnectionError::NotConnected)?;
        let state = con.get_state().map_err(|e| Error::Internal(e.to_string()))?;

        // Clear existing state
        self.server_state.users.clear();
        self.server_state.channels.clear();

        // Sync server info
        self.server_state.server.name = state.server.name.clone();
        self.server_state.server.welcome_message = Some(state.server.welcome_message.clone());
        self.server_state.server.platform = state.server.platform.clone();
        self.server_state.server.version = state.server.version.clone();
        self.server_state.server.max_clients = state.server.max_clients as u32;
        self.server_state.server.icon_id = state.server.icon.0 as i64;

        // Sync all channels (preserve computed permission_hints)
        let old_hints: HashMap<u64, u64> = self.server_state.channels.iter()
            .filter(|(_, ch)| ch.permission_hints != 0)
            .map(|(&id, ch)| (id, ch.permission_hints))
            .collect();
        for (_, ts_channel) in &state.channels {
            let mut channel = ts_channel_to_channel(ts_channel);
            if channel.permission_hints == 0 {
                if let Some(&hints) = old_hints.get(&channel.id) {
                    channel.permission_hints = hints;
                }
            }
            self.server_state.channels.insert(channel.id, channel);
        }

        // Sync all clients
        for (_, ts_client) in &state.clients {
            let user = ts_client_to_user(ts_client);
            self.server_state.users.insert(user.id, user);
        }

        // Update our client ID
        self.client_id = Some(state.own_client.0);

        // Update our channel ID
        if let Some(our_client) = state.clients.get(&state.own_client) {
            self.channel_id = Some(our_client.channel.0);
        }

        info!(
            "State synced: {} users, {} channels",
            self.server_state.users.len(),
            self.server_state.channels.len()
        );

        Ok(())
    }

    /// Process an incoming message event (non-book protocol messages)
    ///
    /// Note: Text messages and pokes arrive via `BookEvents` as `Event::Message`,
    /// not here. This handles remaining protocol messages like channellistfinished.
    fn process_message_event(&mut self, msg: InMessage) -> Option<Event> {
        log::info!("process_message_event: cmd={}", msg.get_command_name());
        match msg {
            InMessage::FileList(file_list) => {
                let count = file_list.iter().count();
                log::info!("Received FileList with {} entries", count);
                if let Some((_, _, ref mut entries)) = self.pending_file_list {
                    for part in file_list.iter() {
                        log::info!("  file entry: name={}, size={}, is_file={}", part.name, part.size, part.is_file);
                        entries.push(FileEntry {
                            name: part.name.clone(),
                            size: part.size,
                            datetime: part.date_time.unix_timestamp(),
                            is_file: part.is_file,
                        });
                    }
                } else {
                    log::warn!("Received FileList but no pending_file_list!");
                }
                None
            }
            InMessage::FileListFinished(_finished) => {
                log::info!("Received FileListFinished");
                if let Some((channel_id, path, entries)) = self.pending_file_list.take() {
                    log::info!("File list complete: channel={}, path={}, {} files", channel_id, path, entries.len());
                    let event = Event::FileListReceived { channel_id, path, files: entries };
                    let _ = self.event_tx.send(event.clone());
                    Some(event)
                } else {
                    log::warn!("FileListFinished but no pending_file_list");
                    None
                }
            }
            InMessage::PermOverview(ref perm) => {
                if let Some((channel_id, ref mut perms)) = self.pending_perm_overview {
                    for part in perm.iter() {
                        let perm_id = part.permission_id.0;
                        let value = part.permission_value;
                        // Keep the best value: -1 (unlimited) wins, otherwise take max
                        let entry = perms.entry(perm_id).or_insert(0);
                        if *entry != -1 {
                            if value == -1 || value > *entry {
                                *entry = value;
                            }
                        }
                    }
                    // Compute hints immediately — tsclientlib consumes the terminating
                    // "error id=0" internally, so CommandError never fires for permoverview.
                    let hints = compute_file_permission_hints(perms);
                    log::info!("Channel {} permission_hints computed: {:#x} ({} perms)", channel_id, hints, perms.len());
                    let cid = channel_id;
                    // Clear pending state
                    self.pending_perm_overview = None;
                    // Update the channel in server state
                    if let Some(ch) = self.server_state.channels.get_mut(&cid) {
                        ch.permission_hints = hints;
                    }
                    let event = Event::ChannelPermissionsUpdated { channel_id: cid, permission_hints: hints };
                    let _ = self.event_tx.send(event.clone());
                    return Some(event);
                }
                None
            }
            InMessage::ClientUpdated(ref updated) => {
                // Log how the server sees our own away flag. TS3 servers do not
                // echo our own `clientupdate` back to us (measured: no such
                // message arrives after `client_away`), so this is diagnostic
                // only - the client never fights the server by re-sending here.
                // Re-sending on every mismatch would spend command budget and can
                // trip the server's flood protection (ClientIsFlooding), which
                // rejects *all* following commands for a while.
                if let (Some(self_id), Some((want_away, _))) =
                    (self.client_id, self.self_away.clone())
                {
                    for part in updated.iter() {
                        if part.client_id.0 != self_id {
                            continue;
                        }
                        if let Some(is_away) = part.is_away {
                            log::info!(
                                "Server reports own away={} (we want {})",
                                is_away, want_away
                            );
                        }
                    }
                }
                None
            }
            InMessage::CommandError(ref err) => {
                for part in err.iter() {
                    log::info!("CommandError: id={:?}, msg={}", part.id, part.message);
                }
                // Handle "database empty result" (1281) — server sends this when a
                // directory has no files instead of FileListFinished.
                if self.pending_file_list.is_some() {
                    let is_empty_result = err.iter().any(|part| part.id == TsError::DatabaseEmptyResult);
                    if is_empty_result {
                        if let Some((channel_id, path, entries)) = self.pending_file_list.take() {
                            log::info!("File list empty dir: channel={}, path={}", channel_id, path);
                            let event = Event::FileListReceived { channel_id, path, files: entries };
                            let _ = self.event_tx.send(event.clone());
                            return Some(event);
                        }
                    }
                }
                // Handle permoverview failure (e.g. permission denied).
                // Success case is handled directly in InMessage::PermOverview.
                if self.pending_perm_overview.is_some() {
                    let is_error = err.iter().any(|part| (part.id as u32) != 0);
                    if is_error {
                        if let Some((channel_id, _)) = self.pending_perm_overview.take() {
                            log::info!("permoverview failed for channel {}, defaulting to browse/download only", channel_id);
                            let hints = HINT_JOIN | HINT_FILE_BROWSE | HINT_FILE_DOWNLOAD;
                            if let Some(ch) = self.server_state.channels.get_mut(&channel_id) {
                                ch.permission_hints = hints;
                            }
                            let event = Event::ChannelPermissionsUpdated { channel_id, permission_hints: hints };
                            let _ = self.event_tx.send(event.clone());
                            return Some(event);
                        }
                    }
                }
                // Emit CommandError event for non-trivial errors (skip id=0 "ok")
                for part in err.iter() {
                    let error_id = part.id as u32;
                    if error_id != 0 {
                        let event = Event::CommandError {
                            error_id,
                            message: part.message.clone(),
                        };
                        let _ = self.event_tx.send(event);
                    }
                }
                None
            }
            _ => {
                log::debug!("Unhandled MessageEvent: {:?}", msg.get_command_name());
                None
            }
        }
    }

    /// Dispatch an event to all handlers
    async fn dispatch_event(&self, event: Event) {
        for handler in &self.handlers {
            handler.on_event(&event).await;
        }
    }

    /// Add an event handler
    pub fn add_handler(&mut self, handler: Arc<dyn EventHandler>) {
        self.handlers.push(handler);
    }

    /// Get the current connection state
    pub fn state(&self) -> ConnectionState {
        self.state.clone()
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.state.is_connected()
    }

    /// Get our client ID
    pub fn client_id(&self) -> Option<u16> {
        self.client_id
    }

    /// Get our current channel ID
    pub fn channel_id(&self) -> Option<u64> {
        self.channel_id
    }

    /// Get a snapshot of the server state
    pub fn server_state(&self) -> &ServerState {
        &self.server_state
    }

    /// Build a ServerInfo snapshot from the live tsclientlib state
    ///
    /// `clients_online`, `channels_online` and `uptime` only exist in the
    /// book's `optional_data`, which the server pushes via
    /// `notifyserverupdated` after `send_server_variables()`. Falls back to
    /// the cached `server_state` when not connected.
    pub fn server_info(&self) -> ServerInfo {
        let mut info = self.server_state.server.clone();
        if let Some(con) = self.connection.as_ref() {
            if let Ok(state) = con.get_state() {
                let server = &state.server;
                info.name = server.name.clone();
                info.welcome_message = Some(server.welcome_message.clone());
                info.platform = server.platform.clone();
                info.version = server.version.clone();
                info.max_clients = server.max_clients as u32;
                info.icon_id = server.icon.0 as i64;
                if let Some(opt) = &server.optional_data {
                    info.clients_online = opt.client_count as u32;
                    info.channels_online = opt.channel_count as u32;
                    info.uptime = opt.uptime.whole_seconds().max(0) as u64;
                }
            }
        }
        info
    }

    /// Get all channels from tsclientlib state
    pub fn channels(&self) -> Vec<Channel> {
        self.server_state.channels.values().cloned().collect()
    }

    /// Get all users from tsclientlib state
    pub fn users(&self) -> Vec<User> {
        let mut users: Vec<User> = self.server_state.users.values().cloned().collect();
        // Apply local mute override for self — tsclientlib's state book
        // doesn't reflect our clientupdate, so any re-read from it will
        // have stale input_muted. We always apply our known state.
        if let (Some(client_id), Some(muted)) = (self.client_id, self.self_input_muted) {
            if let Some(user) = users.iter_mut().find(|u| u.id == client_id) {
                user.is_input_muted = muted;
            }
        }
        if let (Some(client_id), Some(muted)) = (self.client_id, self.self_output_muted) {
            if let Some(user) = users.iter_mut().find(|u| u.id == client_id) {
                user.is_output_muted = muted;
            }
        }
        // Same for the nickname: a clientupdate is never echoed back to the
        // sender, so the book still holds the name we connected with.
        if let (Some(client_id), Some(nickname)) = (self.client_id, self.self_nickname.as_ref()) {
            if let Some(user) = users.iter_mut().find(|u| u.id == client_id) {
                user.nickname = nickname.clone();
            }
        }
        // Away status is a clientupdate too, so it needs the same treatment:
        // without it the member list would show us online right after we set
        // an away message.
        if let (Some(client_id), Some((away, message))) = (self.client_id, self.self_away.as_ref())
        {
            if let Some(user) = users.iter_mut().find(|u| u.id == client_id) {
                user.is_away = *away;
                user.away_message = if *away { message.clone() } else { None };
            }
        }
        users
    }

    /// Get all users directly from tsclientlib's connection state.
    ///
    /// This bypasses `server_state` and reads directly from the live
    /// connection book, which may have users that `server_state` missed.
    pub fn users_from_connection(&self) -> Vec<User> {
        let con = match self.connection.as_ref() {
            Some(c) => c,
            None => return Vec::new(),
        };
        let state = match con.get_state() {
            Ok(s) => s,
            Err(e) => {
                warn!("users_from_connection: get_state failed: {}", e);
                return Vec::new();
            }
        };
        info!(
            "users_from_connection: tsclientlib state has {} clients, {} channels",
            state.clients.len(),
            state.channels.len()
        );
        let mut users: Vec<User> = state.clients.values().map(|c| ts_client_to_user(c)).collect();
        // Apply local mute override (same as users())
        if let (Some(client_id), Some(muted)) = (self.client_id, self.self_input_muted) {
            if let Some(user) = users.iter_mut().find(|u| u.id == client_id) {
                user.is_input_muted = muted;
            }
        }
        if let (Some(client_id), Some(muted)) = (self.client_id, self.self_output_muted) {
            if let Some(user) = users.iter_mut().find(|u| u.id == client_id) {
                user.is_output_muted = muted;
            }
        }
        // Same for the nickname: a clientupdate is never echoed back to the
        // sender, so the book still holds the name we connected with.
        if let (Some(client_id), Some(nickname)) = (self.client_id, self.self_nickname.as_ref()) {
            if let Some(user) = users.iter_mut().find(|u| u.id == client_id) {
                user.nickname = nickname.clone();
            }
        }
        // Away status is a clientupdate too, so it needs the same treatment:
        // without it the member list would show us online right after we set
        // an away message.
        if let (Some(client_id), Some((away, message))) = (self.client_id, self.self_away.as_ref())
        {
            if let Some(user) = users.iter_mut().find(|u| u.id == client_id) {
                user.is_away = *away;
                user.away_message = if *away { message.clone() } else { None };
            }
        }
        users
    }

    /// Get a specific channel
    pub fn channel(&self, id: u64) -> Option<Channel> {
        self.server_state.channels.get(&id).cloned()
    }

    /// Get a specific user
    pub fn user(&self, id: u16) -> Option<User> {
        self.server_state.users.get(&id).cloned()
    }

    /// Get current connection quality metrics (RTT, packet loss, bandwidth).
    ///
    /// Returns `None` while not connected to a server.
    pub fn get_network_stats(&self) -> Option<NetworkStats> {
        let con = self.connection.as_ref()?;
        let stats = con.get_network_stats().ok()?;
        // PacketStat order: InControl, InKeepalive, InSpeech,
        // OutControl, OutKeepalive, OutSpeech
        let last_second = stats.get_last_second_bytes();
        let received: u32 = last_second[PacketStat::InControl as usize]
            + last_second[PacketStat::InKeepalive as usize]
            + last_second[PacketStat::InSpeech as usize];
        let sent: u32 = last_second[PacketStat::OutControl as usize]
            + last_second[PacketStat::OutKeepalive as usize]
            + last_second[PacketStat::OutSpeech as usize];
        // Same packet classes as get_packetloss()'s denominator:
        // voice-in + ping/pong-in + ack-in + ack/command-out
        let loss_observed = stats.total_packets[PacketStat::InSpeech as usize]
            + stats.total_packets[PacketStat::InKeepalive as usize]
            + stats.total_packets[PacketStat::InControl as usize]
            + stats.total_packets[PacketStat::OutControl as usize];
        Some(NetworkStats {
            rtt_ms: stats.rtt.as_secs_f64() * 1000.0,
            rtt_dev_ms: stats.rtt_dev.as_secs_f64() * 1000.0,
            packet_loss: stats.get_packetloss(),
            packet_loss_in: stats.get_packetloss_s2c_total(),
            bytes_received_per_sec: received as u64,
            bytes_sent_per_sec: sent as u64,
            loss_observed_total: loss_observed,
        })
    }

    /// Move to a channel
    pub fn move_to_channel(&mut self, channel_id: u64) -> Result<()> {
        self.move_to_channel_with_password(channel_id, None)
    }

    /// Move to a channel with password
    pub fn move_to_channel_with_password(
        &mut self,
        channel_id: u64,
        password: Option<String>,
    ) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        // Get our client ID from the state
        let our_client_id = con
            .get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .own_client;

        debug!("Moving client {} to channel {}", our_client_id.0, channel_id);

        // Create the clientmove command
        let mut cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "clientmove",
        );
        cmd.write_arg("clid", &our_client_id.0);
        cmd.write_arg("cid", &channel_id);

        // Add password if provided
        if let Some(pwd) = password {
            let encoded_pwd = tsproto_types::crypto::encode_password(pwd.as_bytes());
            cmd.write_arg("cpw", &encoded_pwd);
        }

        // Send the command using OutCommandExt trait
        cmd.send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        self.channel_id = Some(channel_id);
        Ok(())
    }

    /// Send a message to the server
    pub fn send_server_message(&mut self, message: impl Into<String>) -> Result<()> {
        let msg = message.into();
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        debug!("Sending server message: {}", msg);

        // Get state and create message command, then send it
        con.get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .send_message(TsMessageTarget::Server, &msg)
            .send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(())
    }

    /// Send a message to the current channel
    pub fn send_channel_message(&mut self, message: impl Into<String>) -> Result<()> {
        let msg = message.into();
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        debug!("Sending channel message: {}", msg);

        // Get state and create message command, then send it
        con.get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .send_message(TsMessageTarget::Channel, &msg)
            .send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(())
    }

    /// Send a private message to a user
    pub fn send_private_message(&mut self, user_id: u16, message: impl Into<String>) -> Result<()> {
        let msg = message.into();
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        debug!("Sending private message to {}: {}", user_id, msg);

        // Get state and create message command, then send it
        con.get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .send_message(TsMessageTarget::Client(ClientId(user_id)), &msg)
            .send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(())
    }

    /// Send a poke to a user
    pub fn send_poke(&mut self, user_id: u16, message: impl Into<String>) -> Result<()> {
        let msg = message.into();
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        debug!("Sending poke to {}: {}", user_id, msg);

        con.get_state()
            .map_err(|e| Error::Internal(e.to_string()))?
            .send_message(TsMessageTarget::Poke(ClientId(user_id)), &msg)
            .send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        Ok(())
    }

    /// Send audio data
    pub fn send_audio(&mut self, data: &[u8], codec: AudioCodec) -> Result<()> {
        // Server groups are routed by the server (one group-whisper packet
        // each), so they never expand into a client list and never age: whoever
        // holds the group when a packet arrives hears it, late joiners included.
        // Explicit clients are resolved here instead, because a client id is
        // only meaningful for the session that handed it out.
        let whisper_clients = self.resolve_whisper_clients();
        let whisper_active = !self.whisper_clients.is_empty()
            || !self.whisper_channels.is_empty()
            || !self.whisper_groups.is_empty();
        let routes = plan_audio_routes(
            whisper_active,
            &whisper_clients,
            &self.whisper_channels,
            &self.whisper_groups,
        );

        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        if !con.can_send_audio() {
            return Err(Error::Internal("Cannot send audio".to_string()));
        }

        let sequence = self.audio_sequence;
        self.audio_sequence = self.audio_sequence.wrapping_add(1);
        let codec_type = audio_codec_to_codec_type(codec);

        for route in routes {
            let audio_data = match route {
                AudioRoute::Channel => AudioData::C2S {
                    id: sequence,
                    codec: codec_type,
                    data,
                },
                AudioRoute::TargetList { clients, channels } => AudioData::C2SWhisper {
                    id: sequence,
                    codec: codec_type,
                    channels,
                    clients,
                    data,
                },
                AudioRoute::ServerGroup(group_id) => AudioData::C2SWhisperNew {
                    id: sequence,
                    codec: codec_type,
                    whisper_type: GROUP_WHISPER_TYPE_SERVER_GROUP,
                    target: GROUP_WHISPER_TARGET_ALL_CHANNELS,
                    target_id: group_id,
                    data,
                },
            };
            let packet = OutAudio::new(&audio_data);
            con.send_audio(packet)
                .map_err(|e| Error::Internal(e.to_string()))?;
        }

        Ok(())
    }

    /// Set the whisper target set. Voice captured afterwards is sent as
    /// whisper packets to these clients/channels/server groups instead of the
    /// current channel; passing three empty lists switches back to channel talk.
    ///
    /// Client and channel targets are per-packet ids - resolve them from the
    /// current user list at call time, they don't follow reconnects. Server
    /// groups are different: the group id is kept and sent as a group-whisper
    /// packet, so the target survives peer reconnects and membership changes.
    pub fn set_whisper_targets(
        &mut self,
        clients: Vec<u16>,
        channels: Vec<u64>,
        groups: Vec<u64>,
    ) -> Result<()> {
        self.whisper_clients = clients;
        self.whisper_channels = channels;
        self.whisper_groups = groups;
        if self.whisper_clients.is_empty()
            && self.whisper_channels.is_empty()
            && self.whisper_groups.is_empty()
        {
            debug!("Whisper mode off, back to channel talk");
        } else {
            debug!(
                "Whisper mode on: clients={:?}, channels={:?}, groups={:?}",
                self.whisper_clients, self.whisper_channels, self.whisper_groups
            );
        }
        Ok(())
    }

    /// Client ids that must receive the current whisper, sorted and deduplicated.
    ///
    /// Explicit client targets are kept as configured: the server drops packets
    /// for a client id that has vanished, which is the honest outcome. Server
    /// groups are deliberately *not* expanded here - they travel as
    /// group-whisper packets and are resolved by the server.
    fn resolve_whisper_clients(&self) -> Vec<u16> {
        let mut clients = self.whisper_clients.clone();
        clients.sort_unstable();
        clients.dedup();
        clients
    }

    /// Server groups the local book knows, as `(id, name)` sorted by id.
    ///
    /// The list is requested on connect (`servergrouplist`); until the server
    /// answers it is empty. Names are what the whisper list shows - the client
    /// list only carries group ids.
    pub fn list_server_groups(&self) -> Vec<(u64, String)> {
        let Some(con) = self.connection.as_ref() else {
            return Vec::new();
        };
        let Ok(state) = con.get_state() else {
            return Vec::new();
        };
        let mut groups: Vec<(u64, String)> = state
            .server_groups
            .iter()
            .map(|(id, group)| (id.0, group.name.clone()))
            .collect();
        groups.sort_by_key(|(id, _)| *id);
        groups
    }

    /// Ask the server for its group list (`notifyservergrouplist`).
    pub fn request_server_group_list(&mut self) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "servergrouplist",
        );
        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;

        debug!("Requested server group list");
        Ok(())
    }

    /// Channel groups the local book knows, as `(id, name)` sorted by id.
    ///
    /// Requested on connect (`channelgrouplist`), like the server group list:
    /// a member's `channel_group` is only an id, and the panel that shows it
    /// needs the name the server gave the group.
    pub fn list_channel_groups(&self) -> Vec<(u64, String)> {
        let Some(con) = self.connection.as_ref() else {
            return Vec::new();
        };
        let Ok(state) = con.get_state() else {
            return Vec::new();
        };
        let mut groups: Vec<(u64, String)> = state
            .channel_groups
            .iter()
            .map(|(id, group)| (id.0, group.name.clone()))
            .collect();
        groups.sort_by_key(|(id, _)| *id);
        groups
    }

    /// Ask the server for its channel group list (`notifychannelgrouplist`).
    pub fn request_channel_group_list(&mut self) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "channelgrouplist",
        );
        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;

        debug!("Requested channel group list");
        Ok(())
    }

    /// Download a file from the server
    pub fn download_file(&mut self, channel_id: u64, path: &str) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let handle = con
            .download_file(ChannelId(channel_id), path, None, None)
            .map_err(|e| Error::Internal(e.to_string()))?;

        self.pending_downloads
            .insert(handle.0, (channel_id, path.to_string()));

        debug!("Started download: channel={}, path={}", channel_id, path);
        Ok(())
    }

    /// Upload a file to the server
    pub fn upload_file(&mut self, channel_id: u64, path: &str, data: &[u8], overwrite: bool) -> Result<()> {
        let con = self.connection.as_mut().ok_or(ConnectionError::NotConnected)?;
        let handle = con.upload_file(
            ChannelId(channel_id), path, None, data.len() as u64, overwrite, false,
        ).map_err(|e| Error::Internal(e.to_string()))?;
        self.pending_uploads.insert(handle.0, (channel_id, path.to_string(), data.to_vec()));
        debug!("Started upload: channel={}, path={}, size={}", channel_id, path, data.len());
        Ok(())
    }

    /// Request the file list for a channel directory
    pub fn list_files(&mut self, channel_id: u64, path: &str) -> Result<()> {
        let con = self.connection.as_mut().ok_or(ConnectionError::NotConnected)?;
        let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "ftgetfilelist");
        cmd.write_arg("cid", &channel_id);
        cmd.write_arg("cpw", &"");
        cmd.write_arg("path", &path);
        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;
        self.pending_file_list = Some((channel_id, path.to_string(), Vec::new()));
        debug!("Requested file list: channel={}, path={}", channel_id, path);
        Ok(())
    }

    /// Query effective permissions for the current user in a channel.
    /// Sends `permoverview cldbid=X cid=Y` and collects responses asynchronously.
    /// Results arrive as ChannelPermissionsUpdated events.
    pub fn query_channel_permissions(&mut self, channel_id: u64) -> Result<()> {
        let con = self.connection.as_mut().ok_or(ConnectionError::NotConnected)?;
        let state = con.get_state().map_err(|e| Error::Internal(e.to_string()))?;
        let our_client = state.clients.get(&state.own_client)
            .ok_or(Error::Internal("Own client not found".to_string()))?;
        let cldbid = our_client.database_id.0;
        log::info!("query_channel_permissions: cldbid={}, cid={}", cldbid, channel_id);
        let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "permoverview");
        cmd.write_arg("cldbid", &cldbid);
        cmd.write_arg("cid", &channel_id);
        cmd.write_arg("permid", &0u32);
        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;
        self.pending_perm_overview = Some((channel_id, HashMap::new()));
        Ok(())
    }

    /// Delete a file on the server
    pub fn delete_file(&mut self, channel_id: u64, name: &str) -> Result<()> {
        let con = self.connection.as_mut().ok_or(ConnectionError::NotConnected)?;
        let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "ftdeletefile");
        cmd.write_arg("cid", &channel_id);
        cmd.write_arg("cpw", &"");
        cmd.write_arg("name", &name);
        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;
        Ok(())
    }

    /// Rename a file on the server
    pub fn rename_file(&mut self, channel_id: u64, old_name: &str, new_name: &str) -> Result<()> {
        let con = self.connection.as_mut().ok_or(ConnectionError::NotConnected)?;
        let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "ftrenamefile");
        cmd.write_arg("cid", &channel_id);
        cmd.write_arg("cpw", &"");
        cmd.write_arg("tcid", &channel_id);
        cmd.write_arg("tcpw", &"");
        cmd.write_arg("oldname", &old_name);
        cmd.write_arg("newname", &new_name);
        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;
        Ok(())
    }

    /// Create a directory on the server
    pub fn create_directory(&mut self, channel_id: u64, dirname: &str) -> Result<()> {
        let con = self.connection.as_mut().ok_or(ConnectionError::NotConnected)?;
        let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "ftcreatedir");
        cmd.write_arg("cid", &channel_id);
        cmd.write_arg("cpw", &"");
        cmd.write_arg("dirname", &dirname);
        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;
        Ok(())
    }

    /// The streams that can be watched: those of clients in our channel
    pub fn streams(&self) -> Vec<StreamInfo> {
        self.shown_streams.values().cloned().collect()
    }

    /// Ask to watch a stream. The streamer's answer arrives as a
    /// `StreamJoinResponse` event, with its WebRTC offer when it accepts.
    pub fn join_stream(&mut self, client_id: u16, stream_id: &str) -> Result<()> {
        self.send_join_stream_request(client_id, stream_id, false)
    }

    /// Stop watching a stream
    pub fn leave_stream(&mut self, client_id: u16, stream_id: &str) -> Result<()> {
        self.send_join_stream_request(client_id, stream_id, true)
    }

    fn send_join_stream_request(&mut self, client_id: u16, stream_id: &str, remove: bool) -> Result<()> {
        let con = self.connection.as_mut().ok_or(ConnectionError::NotConnected)?;
        let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "joinstreamrequest");
        cmd.write_arg("clid", &client_id);
        cmd.write_arg("id", &stream_id);
        cmd.write_arg("msg", &"");
        cmd.write_arg("is_remove", &u8::from(remove));
        cmd.send_with_result(con).map_err(|e| Error::Internal(e.to_string()))?;
        Ok(())
    }

    /// Send WebRTC signalling (an answer or an ICE candidate, as JSON in
    /// TeamSpeak's `{"cmd":…,"args":…}` form) to a streamer
    pub fn send_stream_signaling(&mut self, client_id: u16, stream_id: &str, json: &str) -> Result<()> {
        let con = self.connection.as_mut().ok_or(ConnectionError::NotConnected)?;
        let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "streamsignaling");
        cmd.write_arg("clid", &client_id);
        cmd.write_arg("id", &stream_id);
        cmd.write_arg("json", &json);
        cmd.send_with_result(con).map_err(|e| Error::Internal(e.to_string()))?;
        Ok(())
    }

    /// Handle the stream commands in a raw command from the server
    fn process_raw_command(&mut self, raw: &str) -> Vec<Event> {
        let mut events = Vec::new();
        for command in streams::parse(raw) {
            match command {
                StreamCommand::Started(info) => {
                    self.streaming_clients.insert(info.client_id);
                    self.stream_info_requested.remove(&info.client_id);
                    self.known_streams.insert(info.id.clone(), info);
                }
                StreamCommand::ClientStreaming { client_id, streaming: true } => {
                    self.streaming_clients.insert(client_id);
                }
                StreamCommand::ClientStreaming { client_id, streaming: false } => {
                    self.streaming_clients.remove(&client_id);
                    self.stream_info_requested.remove(&client_id);
                    self.known_streams.retain(|_, s| s.client_id != client_id);
                }
                // The client may still have other streams; its streaming flag says
                StreamCommand::Stopped { stream_id, .. } => {
                    self.known_streams.remove(&stream_id);
                }
                StreamCommand::JoinResponse { client_id, stream_id, accepted, offer } => {
                    events.push(Event::StreamJoinResponse { client_id, stream_id, accepted, offer });
                }
                StreamCommand::Signaling { client_id, stream_id, json } => {
                    events.push(Event::StreamSignaling { client_id, stream_id, json });
                }
            }
        }
        for event in &events {
            let _ = self.event_tx.send(event.clone());
        }
        events
    }

    /// Bring the reported streams in line with our channel: asks for the
    /// details of streams that are not known yet, and reports the streams that
    /// came into or went out of view. Runs after each batch of stream items, so
    /// the book already knows the clients of the batch's commands.
    fn update_streams(&mut self) -> Vec<Event> {
        let Some(con) = self.connection.as_mut() else { return Vec::new() };
        let Ok(state) = con.get_state() else { return Vec::new() };
        let Some(own_channel) = state.clients.get(&state.own_client).map(|c| c.channel) else {
            return Vec::new();
        };
        let in_our_channel: HashSet<u16> = state
            .clients
            .values()
            .filter(|c| c.channel == own_channel && c.id != state.own_client)
            .map(|c| c.id.0)
            .collect();
        let present: HashSet<u16> = state.clients.keys().map(|id| id.0).collect();

        // Forget clients that left the server
        self.streaming_clients.retain(|id| present.contains(id));
        self.stream_info_requested.retain(|id| present.contains(id));
        self.known_streams.retain(|_, s| present.contains(&s.client_id));

        // The server only describes streams in our own channel
        let mut unknown: Vec<u16> = self
            .streaming_clients
            .iter()
            .filter(|id| {
                in_our_channel.contains(id)
                    && !self.known_streams.values().any(|s| s.client_id == **id)
                    && !self.stream_info_requested.contains(id)
            })
            .copied()
            .collect();
        unknown.sort_unstable();
        for chunk in unknown.chunks(16) {
            let mut cmd = OutCommand::new(Direction::C2S, Flags::empty(), PacketType::Command, "requeststreaminfo");
            for (i, client_id) in chunk.iter().enumerate() {
                if i > 0 {
                    cmd.start_new_part();
                }
                cmd.write_arg("clid", client_id);
            }
            match cmd.send_with_result(con) {
                Ok(_) => self.stream_info_requested.extend(chunk),
                Err(e) => warn!("Could not ask for stream info: {}", e),
            }
        }

        let visible: HashMap<String, StreamInfo> = self
            .known_streams
            .iter()
            .filter(|(_, s)| in_our_channel.contains(&s.client_id))
            .map(|(id, info)| (id.clone(), info.clone()))
            .collect();
        let mut events = Vec::new();
        for (id, info) in &self.shown_streams {
            if !visible.contains_key(id) {
                events.push(Event::StreamStopped { client_id: info.client_id, stream_id: id.clone() });
            }
        }
        for (id, info) in &visible {
            if self.shown_streams.get(id) != Some(info) {
                events.push(Event::StreamStarted { stream: info.clone() });
            }
        }
        self.shown_streams = visible;
        for event in &events {
            let _ = self.event_tx.send(event.clone());
        }
        events
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// Disconnect from the server
    pub fn disconnect(&mut self) -> Result<()> {
        self.disconnect_with_reason("Goodbye")
    }

    /// Disconnect from the server with a reason
    pub fn disconnect_with_reason(&mut self, reason: impl Into<String>) -> Result<()> {
        info!("Disconnecting from server");

        if let Some(con) = self.connection.as_mut() {
            let options = DisconnectOptions::new()
                .reason(Reason::Clientdisconnect)
                .message(reason.into());

            con.disconnect(options)
                .map_err(|e| ConnectionError::ConnectionLost(e.to_string()))?;
        }

        // Don't take/drop the connection here — keep it alive so the runtime
        // has time to send the disconnect packet over the network.
        // The connection will be dropped when the Client is dropped (via close/nativeDestroy).

        self.state = ConnectionState::Disconnected;

        let _ = self.event_tx.send(Event::Disconnected {
            reason: "User requested".to_string(),
        });

        Ok(())
    }

    /// Get access to the underlying tsclientlib connection
    pub fn inner(&self) -> Option<&TsConnection> {
        self.connection.as_ref()
    }

    /// Get mutable access to the underlying tsclientlib connection
    pub fn inner_mut(&mut self) -> Option<&mut TsConnection> {
        self.connection.as_mut()
    }

    /// Notify the server of our input muted state
    pub fn set_input_muted(&mut self, muted: bool) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let val: u32 = if muted { 1 } else { 0 };
        let mut cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "clientupdate",
        );
        cmd.write_arg("client_input_muted", &val);

        cmd.send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        info!("Sent clientupdate client_input_muted={}", val);

        // Track locally — tsclientlib's state book won't reflect this
        // because the server doesn't echo clientupdate back to the sender.
        self.self_input_muted = Some(muted);

        // Also patch server_state immediately
        if let Some(client_id) = self.client_id {
            if let Some(user) = self.server_state.users.get_mut(&client_id) {
                user.is_input_muted = muted;
            }
        }

        Ok(())
    }

    /// Notify the server of our output (speaker) muted state, so the others
    /// see that we cannot hear them.
    pub fn set_output_muted(&mut self, muted: bool) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let val: u32 = if muted { 1 } else { 0 };
        let mut cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "clientupdate",
        );
        cmd.write_arg("client_output_muted", &val);

        cmd.send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        info!("Sent clientupdate client_output_muted={}", val);

        // Not echoed back to the sender either; see set_input_muted.
        self.self_output_muted = Some(muted);
        if let Some(client_id) = self.client_id {
            if let Some(user) = self.server_state.users.get_mut(&client_id) {
                user.is_output_muted = muted;
            }
        }

        Ok(())
    }

    /// Change our nickname for this session (`clientupdate client_nickname`).
    ///
    /// Temporary on purpose: the nickname lives server-side only until the
    /// connection ends, so the identity file and the next connect are untouched.
    /// The local book does not get the change echoed back, so it is patched here
    /// and remembered as an override for [`Client::users`].
    pub fn set_nickname(&mut self, nickname: &str) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let mut cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "clientupdate",
        );
        cmd.write_arg("client_nickname", &nickname);

        cmd.send(con)
            .map_err(|e| Error::Internal(e.to_string()))?;

        info!("Sent clientupdate client_nickname={}", nickname);

        self.self_nickname = Some(nickname.to_string());
        if let Some(client_id) = self.client_id {
            if let Some(user) = self.server_state.users.get_mut(&client_id) {
                user.nickname = nickname.to_string();
            }
        }

        Ok(())
    }

    /// Set our own away state (`clientupdate client_away=1` plus an optional
    /// `client_away_message`), i.e. the desktop client's status menu.
    ///
    /// Away is a pure status flag: voice and text keep working, the server just
    /// tells everybody else that we are away (and why). `away = false` clears
    /// both fields. The server does not echo `clientupdate` back to the sender,
    /// so the change is remembered as an override for [`Client::users`].
    pub fn set_away(&mut self, away: bool, message: Option<&str>) -> Result<()> {
        let con = self
            .connection
            .as_mut()
            .ok_or(ConnectionError::NotConnected)?;

        let message = normalize_away_message(away, message);

        let mut cmd = OutCommand::new(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            "clientupdate",
        );
        cmd.write_arg("client_away", &(if away { 1 } else { 0 }));
        if let Some(msg) = message.as_ref() {
            cmd.write_arg("client_away_message", msg);
        }

        cmd.send(con).map_err(|e| Error::Internal(e.to_string()))?;

        // `log::` on purpose: the tracing macros are compiled out of the release
        // build, and this is the line that proves a status command left the app.
        log::info!(
            "Sent clientupdate client_away={} message={:?}",
            away, message
        );

        self.self_away = Some((away, message.clone()));
        if let Some(client_id) = self.client_id {
            if let Some(user) = self.server_state.users.get_mut(&client_id) {
                user.is_away = away;
                user.away_message = message;
            }
        }

        Ok(())
    }
}

/// Convert tsclientlib CodecType to our AudioCodec
fn codec_type_to_audio_codec(codec: CodecType) -> AudioCodec {
    match codec {
        CodecType::SpeexNarrowband => AudioCodec::SpeexNarrowband,
        CodecType::SpeexWideband => AudioCodec::SpeexWideband,
        CodecType::SpeexUltrawideband => AudioCodec::SpeexUltraWideband,
        CodecType::CeltMono => AudioCodec::CeltMono,
        CodecType::OpusVoice => AudioCodec::OpusVoice,
        CodecType::OpusMusic => AudioCodec::OpusMusic,
    }
}

/// Convert our AudioCodec to tsclientlib CodecType
fn audio_codec_to_codec_type(codec: AudioCodec) -> CodecType {
    match codec {
        AudioCodec::SpeexNarrowband => CodecType::SpeexNarrowband,
        AudioCodec::SpeexWideband => CodecType::SpeexWideband,
        AudioCodec::SpeexUltraWideband => CodecType::SpeexUltrawideband,
        AudioCodec::CeltMono => CodecType::CeltMono,
        AudioCodec::OpusVoice => CodecType::OpusVoice,
        AudioCodec::OpusMusic => CodecType::OpusMusic,
    }
}

/// `GroupWhisperType::ServerGroup`: the target id names a server group whose
/// members should hear the packet (ts3protocol.md section 1.8.2.1).
const GROUP_WHISPER_TYPE_SERVER_GROUP: u8 = 0;

/// `GroupWhisperTarget::AllChannels`: the group is targeted wherever its members
/// are, with no channel restriction.
const GROUP_WHISPER_TARGET_ALL_CHANNELS: u8 = 0;

/// How one captured audio frame reaches its listeners.
///
/// A whisper set can mix explicit targets with server groups, and the two use
/// different packets: the legacy list carries clients/channels, while a group
/// needs a `VoiceWhisper` packet with the `Newprotocol` flag, which can only
/// name one group. One frame can therefore produce several packets.
#[derive(Debug, PartialEq, Eq)]
enum AudioRoute {
    /// Normal channel talk.
    Channel,
    /// Whisper to an explicit client/channel list (`C2SWhisper`, legacy format).
    TargetList { clients: Vec<u16>, channels: Vec<u64> },
    /// Whisper to every member of a server group (`C2SWhisperNew`).
    ServerGroup(u64),
}

/// The packets one audio frame has to be sent as.
///
/// [`AudioRoute::Channel`] is returned only when whispering is off, so a
/// configured whisper can never leak into the channel. When whispering is on but
/// nothing is reachable - every explicitly listed client went offline and no
/// group or channel target is left - the frame is aimed at the virtual client 0,
/// which the server drops.
fn plan_audio_routes(
    whisper_active: bool,
    clients: &[u16],
    channels: &[u64],
    groups: &[u64],
) -> Vec<AudioRoute> {
    if !whisper_active {
        return vec![AudioRoute::Channel];
    }

    let mut routes = Vec::new();
    if !clients.is_empty() || !channels.is_empty() {
        routes.push(AudioRoute::TargetList {
            clients: clients.to_vec(),
            channels: channels.to_vec(),
        });
    }
    routes.extend(groups.iter().copied().map(AudioRoute::ServerGroup));

    if routes.is_empty() {
        routes.push(AudioRoute::TargetList {
            clients: vec![0],
            channels: Vec::new(),
        });
    }
    routes
}

/// Convert tsclientlib Client to our User type
fn ts_client_to_user(client: &TsClient) -> User {
    // Convert UID bytes to base64 string
    let uid = client.uid.as_ref()
        .map(|u| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &u.0))
        .unwrap_or_default();

    // Check client type
    let client_type = if matches!(client.client_type, TsClientType::Query { .. }) { 1 } else { 0 };

    User {
        id: client.id.0,
        uid,
        database_id: client.database_id.0,
        channel_id: client.channel.0,
        nickname: client.name.clone(),
        client_type,
        is_talking: false, // Tracked separately via audio
        is_input_muted: client.input_muted,
        is_output_muted: client.output_muted,
        has_input_hardware: client.input_hardware_enabled,
        has_output_hardware: client.output_hardware_enabled,
        is_away: client.away_message.is_some(),
        away_message: client.away_message.clone(),
        is_recording: client.is_recording,
        is_priority_speaker: client.is_priority_speaker,
        is_channel_commander: client.is_channel_commander,
        talk_power: client.talk_power,
        is_talker: client.talk_power_granted,
        server_groups: client.server_groups.iter().map(|g| g.0).collect(),
        channel_group: client.channel_group.0,
        platform: client.optional_data.as_ref().map(|o| o.platform.clone()).unwrap_or_default(),
        version: client.optional_data.as_ref().map(|o| o.version.clone()).unwrap_or_default(),
        country: None, // Not directly available in tsclientlib
        description: None,
        avatar_id: if client.avatar_hash.is_empty() { None } else { Some(client.avatar_hash.clone()) },
        icon_id: client.icon.0 as i64,
        idle_time: 0,
        connected_time: 0,
    }
}

/// Convert tsclientlib Channel to our Channel type
fn ts_channel_to_channel(channel: &TsChannel) -> Channel {
    let (is_permanent, is_semi_permanent) = match channel.channel_type {
        TsChannelType::Permanent => (true, false),
        TsChannelType::SemiPermanent => (false, true),
        TsChannelType::Temporary => (false, false),
    };

    // Extract description from optional data
    let description = channel.optional_data.as_ref().map(|o| o.description.clone());

    // Convert MaxClients enum to i32 (-1 for unlimited)
    let max_clients = channel.max_clients
        .map(|m| match m {
            tsproto_types::MaxClients::Unlimited => -1,
            tsproto_types::MaxClients::Inherited => -1,
            tsproto_types::MaxClients::Limited(n) => n as i32,
        })
        .unwrap_or(-1);

    let max_family_clients = channel.max_family_clients
        .map(|m| match m {
            tsproto_types::MaxClients::Unlimited => -1,
            tsproto_types::MaxClients::Inherited => -1,
            tsproto_types::MaxClients::Limited(n) => n as i32,
        })
        .unwrap_or(-1);

    Channel {
        id: channel.id.0,
        parent_id: channel.parent.0,
        name: channel.name.clone(),
        topic: channel.topic.clone(),
        description,
        order: channel.order.0 as i32,
        is_permanent,
        is_semi_permanent,
        is_default: channel.is_default.unwrap_or(false),
        has_password: channel.has_password.unwrap_or(false),
        codec: ts_codec_to_u8(channel.codec),
        codec_quality: channel.codec_quality.unwrap_or(7),
        max_clients,
        max_family_clients,
        needed_talk_power: channel.needed_talk_power.unwrap_or(0),
        icon_id: channel.icon.map(|i| i.0 as i64).unwrap_or(0),
        is_subscribed: channel.subscribed,
        permission_hints: {
            let hints = channel.permission_hints.map(|h| h.bits() as u64).unwrap_or(0);
            if hints != 0 {
                log::info!("Channel {} ({}) permission_hints = {:#x}", channel.name, channel.id.0, hints);
            }
            hints
        },
    }
}

/// Convert tsclientlib Codec to u8
fn ts_codec_to_u8(codec: TsCodec) -> u8 {
    match codec {
        TsCodec::SpeexNarrowband => 0,
        TsCodec::SpeexWideband => 1,
        TsCodec::SpeexUltrawideband => 2,
        TsCodec::CeltMono => 3,
        TsCodec::OpusVoice => 4,
        TsCodec::OpusMusic => 5,
    }
}

/// Permission IDs for file operations (from Permissions.csv, 0-indexed from line 2)
const PERM_FT_FILE_UPLOAD_POWER: u32 = 235;
const PERM_FT_NEEDED_FILE_UPLOAD_POWER: u32 = 236;
const PERM_FT_FILE_DOWNLOAD_POWER: u32 = 237;
const PERM_FT_NEEDED_FILE_DOWNLOAD_POWER: u32 = 238;
const PERM_FT_FILE_DELETE_POWER: u32 = 239;
const PERM_FT_NEEDED_FILE_DELETE_POWER: u32 = 240;
const PERM_FT_FILE_RENAME_POWER: u32 = 241;
const PERM_FT_NEEDED_FILE_RENAME_POWER: u32 = 242;
const PERM_FT_FILE_BROWSE_POWER: u32 = 243;
const PERM_FT_NEEDED_FILE_BROWSE_POWER: u32 = 244;
const PERM_FT_DIRECTORY_CREATE_POWER: u32 = 245;
const PERM_FT_NEEDED_DIRECTORY_CREATE_POWER: u32 = 246;

/// ChannelPermissionHint bitflags (matches Enums.toml / Channel.java constants)
const HINT_JOIN: u64 = 1;
const HINT_FILE_UPLOAD: u64 = 64;
const HINT_FILE_DOWNLOAD: u64 = 128;
const HINT_FILE_DELETE: u64 = 256;
const HINT_FILE_RENAME: u64 = 512;
const HINT_FILE_BROWSE: u64 = 1024;
const HINT_FILE_DIRECTORY_CREATE: u64 = 2048;

/// Compute ChannelPermissionHint bitflags from a permoverview result.
/// For each file operation, the user can do it if their power >= the needed power.
/// Permissions not present in the map default to 0.
fn compute_file_permission_hints(perms: &HashMap<u32, i32>) -> u64 {
    let mut hints: u64 = HINT_JOIN; // If we're in the channel, we can join

    let can = |power_id: u32, needed_id: u32| -> bool {
        match perms.get(&power_id) {
            Some(&-1) => true, // unlimited (admin)
            Some(&power) => power >= perms.get(&needed_id).copied().unwrap_or(0),
            // Power absent from permoverview = not explicitly restricted.
            // Show the button; the server enforces the real check anyway.
            None => true,
        }
    };

    if can(PERM_FT_FILE_UPLOAD_POWER, PERM_FT_NEEDED_FILE_UPLOAD_POWER) {
        hints |= HINT_FILE_UPLOAD;
    }
    if can(PERM_FT_FILE_DOWNLOAD_POWER, PERM_FT_NEEDED_FILE_DOWNLOAD_POWER) {
        hints |= HINT_FILE_DOWNLOAD;
    }
    if can(PERM_FT_FILE_DELETE_POWER, PERM_FT_NEEDED_FILE_DELETE_POWER) {
        hints |= HINT_FILE_DELETE;
    }
    if can(PERM_FT_FILE_RENAME_POWER, PERM_FT_NEEDED_FILE_RENAME_POWER) {
        hints |= HINT_FILE_RENAME;
    }
    if can(PERM_FT_FILE_BROWSE_POWER, PERM_FT_NEEDED_FILE_BROWSE_POWER) {
        hints |= HINT_FILE_BROWSE;
    }
    if can(PERM_FT_DIRECTORY_CREATE_POWER, PERM_FT_NEEDED_DIRECTORY_CREATE_POWER) {
        hints |= HINT_FILE_DIRECTORY_CREATE;
    }

    log::info!("compute_file_permission_hints: perms={:?} -> hints={:#x}", perms, hints);
    hints
}

/// Away messages are optional and user-typed: trim what the UI sends, treat a
/// blank one as "no message", and never attach one to the online status.
fn normalize_away_message(away: bool, message: Option<&str>) -> Option<String> {
    if !away {
        return None;
    }
    message
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod whisper_routing_tests {
    use super::{
        plan_audio_routes, AudioRoute, GROUP_WHISPER_TARGET_ALL_CHANNELS,
        GROUP_WHISPER_TYPE_SERVER_GROUP,
    };
    use tsproto_packets::packets::{AudioData, Flags, OutAudio, PacketType};

    #[test]
    fn channel_talk_is_a_single_channel_frame() {
        assert_eq!(plan_audio_routes(false, &[], &[], &[]), vec![AudioRoute::Channel]);
        // A stale client list must not whisper once whispering is off.
        assert_eq!(
            plan_audio_routes(false, &[3], &[7], &[6]),
            vec![AudioRoute::Channel]
        );
    }

    #[test]
    fn explicit_targets_share_one_list_packet() {
        assert_eq!(
            plan_audio_routes(true, &[3, 7], &[42], &[]),
            vec![AudioRoute::TargetList {
                clients: vec![3, 7],
                channels: vec![42],
            }]
        );
    }

    #[test]
    fn each_server_group_gets_its_own_packet() {
        assert_eq!(
            plan_audio_routes(true, &[], &[], &[6, 8]),
            vec![AudioRoute::ServerGroup(6), AudioRoute::ServerGroup(8)]
        );
    }

    #[test]
    fn groups_and_explicit_targets_are_sent_side_by_side() {
        assert_eq!(
            plan_audio_routes(true, &[3], &[], &[6]),
            vec![
                AudioRoute::TargetList { clients: vec![3], channels: vec![] },
                AudioRoute::ServerGroup(6),
            ]
        );
    }

    #[test]
    fn a_whisper_with_every_target_offline_is_dropped_not_broadcast() {
        // Client 0 is the server's virtual client: the packet goes nowhere, which
        // is the point - an empty target list would mean "no whisper routing" and
        // broadcast the microphone to the whole channel.
        assert_eq!(
            plan_audio_routes(true, &[], &[], &[]),
            vec![AudioRoute::TargetList {
                clients: vec![0],
                channels: vec![],
            }]
        );
    }

    /// Pins the group-whisper packet to the layout in ts3protocol.md 1.8.2.1:
    /// `VId u16 | C u8 | TY u8 | TA u8 | U u64`, with the Newprotocol flag set.
    #[test]
    fn group_whisper_packet_matches_the_documented_layout() {
        let data = AudioData::C2SWhisperNew {
            id: 0x1234,
            codec: tsproto_packets::packets::CodecType::OpusVoice,
            whisper_type: GROUP_WHISPER_TYPE_SERVER_GROUP,
            target: GROUP_WHISPER_TARGET_ALL_CHANNELS,
            target_id: 6,
            data: &[9, 9],
        };
        let packet = OutAudio::new(&data);

        assert_eq!(packet.header().packet_type(), PacketType::VoiceWhisper);
        assert!(
            packet.header().flags().contains(Flags::NEWPROTOCOL),
            "group whisper is only understood with the Newprotocol flag"
        );

        let content = packet.content();
        assert_eq!(&content[..2], &0x1234u16.to_be_bytes(), "voice packet id");
        assert_eq!(content[2], 4, "codec id 4 = Opus Voice on the wire");
        assert_eq!(content[3], GROUP_WHISPER_TYPE_SERVER_GROUP, "TY");
        assert_eq!(content[4], GROUP_WHISPER_TARGET_ALL_CHANNELS, "TA");
        assert_eq!(&content[5..13], &6u64.to_be_bytes(), "U = server group id");
        assert_eq!(&content[13..], &[9, 9], "audio payload is passed through");
    }
}

#[cfg(test)]
mod away_status_tests {
    use super::normalize_away_message;

    #[test]
    fn online_status_never_carries_a_message() {
        // Going back online must clear the message, even if the UI still holds
        // the text it sent a minute ago.
        assert_eq!(normalize_away_message(false, Some("lunch")), None);
        assert_eq!(normalize_away_message(false, None), None);
    }

    #[test]
    fn away_message_is_trimmed() {
        assert_eq!(
            normalize_away_message(true, Some("  in a meeting  ")),
            Some("in a meeting".to_string())
        );
    }

    #[test]
    fn blank_away_message_means_no_message() {
        assert_eq!(normalize_away_message(true, Some("   ")), None);
        assert_eq!(normalize_away_message(true, Some("")), None);
        assert_eq!(normalize_away_message(true, None), None);
    }

    #[test]
    fn away_without_message_is_still_away() {
        // The flag and the message are independent: an away client with no
        // message is exactly what the protocol allows.
        assert_eq!(normalize_away_message(true, Some("")), None);
    }
}
