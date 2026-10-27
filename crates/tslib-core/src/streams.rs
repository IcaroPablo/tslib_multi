//! TeamSpeak 6 streams (screen and window sharing), watch-only.
//!
//! A TeamSpeak 6 server announces streams to every client, TeamSpeak 3 protocol
//! ones included, but tsclientlib's declarations do not know the commands. They
//! are read here from the raw command text instead. Watching works like this:
//!
//! 1. The server announces a stream (`notifystreamstarted`), or a client is
//!    flagged with `client_is_streaming=1` and `requeststreaminfo` returns the
//!    stream (`notifystreaminfo`).
//! 2. The viewer sends `joinstreamrequest`. The streamer accepts and the server
//!    passes on its WebRTC offer (`notifyrespondjoinstreamrequest`).
//! 3. Both sides exchange WebRTC signalling (answer, ICE candidates) through the
//!    server as JSON (`streamsignaling` / `notifystreamsignaling`).
//!
//! On a server without a media relay (SFU) the video then flows directly
//! between the streamer and the viewer.

use std::borrow::Cow;
use std::collections::HashMap;

use tsproto_packets::commands::{CommandItem, CommandParser};

/// A stream announced by the server
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// The stream's identifier (a UUID)
    pub id: String,
    /// The streaming client
    pub client_id: u16,
    /// The name the streamer gave it
    pub name: String,
    /// The kind of stream as TeamSpeak numbers it (2 for a screen, 3 for a window)
    pub kind: u32,
    /// The video bitrate the streamer set, in kbit/s
    pub bitrate: u32,
    /// Whether the stream carries audio
    pub has_audio: bool,
}

/// A stream-related command from the server
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamCommand {
    /// A stream started, or `requeststreaminfo` described one
    Started(StreamInfo),
    /// A stream ended
    Stopped { client_id: u16, stream_id: String },
    /// A client's `client_is_streaming` flag
    ClientStreaming { client_id: u16, streaming: bool },
    /// The streamer's answer to our join request, with its WebRTC offer
    JoinResponse { client_id: u16, stream_id: String, accepted: bool, offer: Option<String> },
    /// WebRTC signalling from the streamer, as JSON
    Signaling { client_id: u16, stream_id: String, json: String },
}

/// Reads the stream-related parts of a raw command from the server.
pub(crate) fn parse(raw: &str) -> Vec<StreamCommand> {
    let (name, parser) = CommandParser::new(raw.as_bytes());
    let name = String::from_utf8_lossy(name);
    if !matches!(
        &*name,
        "notifystreamstarted"
            | "notifystreaminfo"
            | "notifystreamstopped"
            | "notifyrespondjoinstreamrequest"
            | "notifystreamsignaling"
            | "initserver"
            | "notifycliententerview"
            | "notifyclientupdated"
    ) {
        return Vec::new();
    }

    let mut result = Vec::new();
    for part in parts(parser) {
        let get = |key: &str| part.get(key).map(|v| v.as_ref());
        let client_id = get("clid").and_then(|v| v.parse::<u16>().ok());
        let stream_id = get("id").map(str::to_owned);
        let command = match (&*name, client_id, stream_id) {
            ("notifystreamstarted" | "notifystreaminfo", Some(client_id), Some(id)) => {
                Some(StreamCommand::Started(StreamInfo {
                    id,
                    client_id,
                    name: get("name").unwrap_or_default().to_owned(),
                    kind: get("type").and_then(|v| v.parse().ok()).unwrap_or(0),
                    bitrate: get("bitrate").and_then(|v| v.parse().ok()).unwrap_or(0),
                    has_audio: get("audio") == Some("1"),
                }))
            }
            ("notifystreamstopped", Some(client_id), Some(stream_id)) => {
                Some(StreamCommand::Stopped { client_id, stream_id })
            }
            ("notifyrespondjoinstreamrequest", Some(client_id), Some(stream_id)) => {
                Some(StreamCommand::JoinResponse {
                    client_id,
                    stream_id,
                    accepted: get("decision") == Some("1"),
                    offer: get("offer").filter(|v| !v.is_empty()).map(str::to_owned),
                })
            }
            ("notifystreamsignaling", Some(client_id), Some(stream_id)) => get("json")
                .map(|json| StreamCommand::Signaling { client_id, stream_id, json: json.to_owned() }),
            (_, Some(client_id), _) => get("client_is_streaming")
                .map(|v| StreamCommand::ClientStreaming { client_id, streaming: v == "1" }),
            _ => None,
        };
        result.extend(command);
    }
    result
}

/// The arguments of each part of a command (parts are separated by `|`).
fn parts(parser: CommandParser<'_>) -> Vec<HashMap<String, Cow<'_, str>>> {
    let mut parts = vec![HashMap::new()];
    for item in parser {
        match item {
            CommandItem::NextCommand => parts.push(HashMap::new()),
            CommandItem::Argument(arg) => {
                if let Ok(value) = arg.value().get_str() {
                    let name = String::from_utf8_lossy(arg.name()).into_owned();
                    parts.last_mut().unwrap().insert(name, value);
                }
            }
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_started() {
        let raw = "notifystreamstarted clid=12 id=afd6a301-5dd1-44a9-9283-581f050d5857 \
                   name=Screen\\s484 type=2 access=1 mode=1 bitrate=50880 viewer_limit=0 audio=0";
        assert_eq!(parse(raw), vec![StreamCommand::Started(StreamInfo {
            id: "afd6a301-5dd1-44a9-9283-581f050d5857".into(),
            client_id: 12,
            name: "Screen 484".into(),
            kind: 2,
            bitrate: 50880,
            has_audio: false,
        })]);
    }

    #[test]
    fn stream_info_rows() {
        let raw = "notifystreaminfo return_code=7 clid=8 id=a name=One type=3 accessibility=1 mode=1 \
                   viewer=1 bitrate=19264 viewer_limit=0 audio=1|clid=9 id=b name=Two type=2 audio=0";
        let started: Vec<_> = parse(raw)
            .into_iter()
            .map(|c| match c {
                StreamCommand::Started(info) => (info.client_id, info.id, info.has_audio),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(started, vec![(8, "a".into(), true), (9, "b".into(), false)]);
    }

    #[test]
    fn stream_stopped() {
        assert_eq!(parse("notifystreamstopped clid=12 id=x reason=1"), vec![StreamCommand::Stopped {
            client_id: 12,
            stream_id: "x".into()
        }]);
    }

    #[test]
    fn join_response_carries_the_offer() {
        let raw = "notifyrespondjoinstreamrequest clid=12 id=x msg decision=1 offer=v=0\\r\\no=-\\s1\\s2";
        assert_eq!(parse(raw), vec![StreamCommand::JoinResponse {
            client_id: 12,
            stream_id: "x".into(),
            accepted: true,
            offer: Some("v=0\r\no=- 1 2".into()),
        }]);
    }

    #[test]
    fn signaling_json_is_unescaped() {
        let raw = r#"notifystreamsignaling clid=12 id=x json={"args":{"mLine":0,"mid":"0","sdp":"candidate:1\s1\sudp"},"cmd":"iceCandidate"}"#;
        assert_eq!(parse(raw), vec![StreamCommand::Signaling {
            client_id: 12,
            stream_id: "x".into(),
            json: r#"{"args":{"mLine":0,"mid":"0","sdp":"candidate:1 1 udp"},"cmd":"iceCandidate"}"#.into(),
        }]);
    }

    #[test]
    fn streaming_flag_of_each_client() {
        let raw = "notifycliententerview cfid=0 ctid=1 clid=8 client_nickname=a client_is_streaming=1\
                   |clid=10 client_nickname=b client_is_streaming=0|clid=11 client_nickname=c";
        assert_eq!(parse(raw), vec![
            StreamCommand::ClientStreaming { client_id: 8, streaming: true },
            StreamCommand::ClientStreaming { client_id: 10, streaming: false },
        ]);
        assert_eq!(parse("notifyclientupdated clid=12 client_is_streaming=1"), vec![
            StreamCommand::ClientStreaming { client_id: 12, streaming: true }
        ]);
    }

    #[test]
    fn other_commands_are_ignored() {
        assert!(parse("notifytextmessage targetmode=2 msg=hi invokerid=3").is_empty());
        assert!(parse("channellist cid=1 channel_name=Geral").is_empty());
    }
}
