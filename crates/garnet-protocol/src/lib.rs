//! Wire-level implementation of the Minecraft Java Edition protocol.
//!
//! This crate knows how to turn packets into bytes and back. It does not know
//! anything about game logic; that lives in `garnet-server`.
//!
//! Layout:
//! - `buffer`  – reading and writing primitive types (VarInt, strings, UUIDs...)
//! - `nbt`     – the NBT binary format used for registry data, text and chunks
//! - `text`    – chat/text components
//! - `framing` – packet length prefix, compression and encryption
//! - `packets` – one module per connection state with the packet structs

pub mod buffer;
pub mod framing;
pub mod nbt;
pub mod packets;
pub mod text;
pub mod types;

pub use buffer::{PacketReader, PacketWriter};
pub use framing::Codec;
pub use packets::{ClientboundPacket, PacketIds, ServerboundPacket, State};
pub use text::Text;
pub use types::*;

/// Errors that can happen while decoding bytes coming from a client.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unexpected end of packet (needed {needed} more bytes)")]
    UnexpectedEof { needed: usize },
    #[error("VarInt is too long")]
    VarIntTooLong,
    #[error("string is not valid UTF-8")]
    InvalidUtf8,
    #[error("string too long: {len} > {max}")]
    StringTooLong { len: usize, max: usize },
    #[error("packet too large: {0} bytes")]
    PacketTooLarge(usize),
    #[error("invalid NBT: {0}")]
    InvalidNbt(String),
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error("unknown packet {state:?} 0x{id:02x}")]
    UnknownPacket { state: State, id: i32 },
    #[error("compression error: {0}")]
    Compression(String),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
