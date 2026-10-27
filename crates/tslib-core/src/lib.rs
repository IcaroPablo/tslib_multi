//! # tslib-core
//!
//! Core TeamSpeak 3 client library providing connection management,
//! identity handling, and protocol abstraction.
//!
//! ## Features
//!
//! - Connection to TeamSpeak 3 servers
//! - Identity creation and management
//! - Event-driven architecture
//! - Async/await support
//!
//! ## Example
//!
//! ```rust,no_run
//! use tslib_core::{Client, ClientConfig, Identity};
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let identity = Identity::create()?;
//!
//!     let config = ClientConfig::builder()
//!         .address("localhost:9987")
//!         .identity(identity)
//!         .nickname("TsLibBot")
//!         .build()?;
//!
//!     let mut client = Client::connect(config)?;
//!
//!     // Handle events...
//!     client.disconnect()?;
//!     Ok(())
//! }
//! ```

pub mod client;
pub mod config;
pub mod connection;
pub mod error;
pub mod events;
pub mod identity;
pub mod state;
pub mod streams;

// Re-exports
pub use client::{Client, FileEntry, NetworkStats};
pub use config::ClientConfig;
pub use connection::{Connection, ConnectionState};
pub use error::{Error, Result};
pub use events::{AudioCodec, Event, EventHandler};
pub use identity::Identity;
pub use state::{Channel, ServerState, User};
pub use streams::StreamInfo;
