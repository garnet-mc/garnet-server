//! Packet framing: length prefixes, optional zlib compression and optional
//! AES/CFB8 encryption.
//!
//! On the wire every packet looks like this:
//!
//! ```text
//! without compression:  [length: VarInt] [packet id: VarInt] [body...]
//! with compression:     [length: VarInt] [uncompressed length: VarInt] [zlib(id + body)]
//!                        ...or uncompressed length 0 followed by plain id + body
//!                        when the payload is below the threshold.
//! ```
//!
//! Encryption, once enabled, is applied to the whole byte stream in both
//! directions with the same shared secret as key and IV.

use crate::buffer::{varint_len, write_varint_to};
use crate::{ProtocolError, Result};
use aes::cipher::KeyIvInit;
use bytes::{Buf, BytesMut};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

type Encryptor = cfb8::Encryptor<aes::Aes128>;
type Decryptor = cfb8::Decryptor<aes::Aes128>;

/// The vanilla limit for a single packet.
pub const MAX_PACKET_SIZE: usize = 2 * 1024 * 1024;

/// Per-connection framing state. Owns the receive buffer.
pub struct Codec {
    incoming: BytesMut,
    compression_threshold: Option<usize>,
    encryptor: Option<Encryptor>,
    decryptor: Option<Decryptor>,
    /// Bytes in `incoming` that have already been decrypted.
    decrypted_len: usize,
}

impl Default for Codec {
    fn default() -> Self {
        Self::new()
    }
}

impl Codec {
    pub fn new() -> Self {
        Self {
            incoming: BytesMut::with_capacity(8192),
            compression_threshold: None,
            encryptor: None,
            decryptor: None,
            decrypted_len: 0,
        }
    }

    /// Turns compression on. Packets at or above `threshold` bytes get zlib'd.
    pub fn enable_compression(&mut self, threshold: usize) {
        self.compression_threshold = Some(threshold);
    }

    /// Turns encryption on with the shared secret negotiated during login.
    pub fn enable_encryption(&mut self, shared_secret: &[u8; 16]) {
        self.encryptor = Some(Encryptor::new(shared_secret.into(), shared_secret.into()));
        self.decryptor = Some(Decryptor::new(shared_secret.into(), shared_secret.into()));
    }

    pub fn compression_enabled(&self) -> bool {
        self.compression_threshold.is_some()
    }

    /// Feed raw bytes received from the socket.
    pub fn push_received(&mut self, bytes: &[u8]) {
        self.incoming.extend_from_slice(bytes);
        if let Some(dec) = &mut self.decryptor {
            let start = self.decrypted_len;
            dec.decrypt(&mut self.incoming[start..]);
        }
        self.decrypted_len = self.incoming.len();
    }

    /// Pulls the next complete packet (packet id + body) out of the receive
    /// buffer, or `None` if more bytes are needed.
    pub fn next_packet(&mut self) -> Result<Option<Vec<u8>>> {
        let Some((frame_len, header_len)) = peek_varint(&self.incoming)? else {
            return Ok(None);
        };
        if frame_len > MAX_PACKET_SIZE {
            return Err(ProtocolError::PacketTooLarge(frame_len));
        }
        if self.incoming.len() < header_len + frame_len {
            return Ok(None);
        }
        self.incoming.advance(header_len);
        let frame = self.incoming.split_to(frame_len);
        self.decrypted_len = self.incoming.len();

        if self.compression_threshold.is_none() {
            return Ok(Some(frame.to_vec()));
        }

        // Compressed framing: first VarInt is the uncompressed size, 0 = not compressed.
        let (data_len, used) = peek_varint(&frame)?.ok_or(ProtocolError::UnexpectedEof { needed: 1 })?;
        let payload = &frame[used..];
        if data_len == 0 {
            return Ok(Some(payload.to_vec()));
        }
        if data_len > MAX_PACKET_SIZE {
            return Err(ProtocolError::PacketTooLarge(data_len));
        }
        let mut out = Vec::with_capacity(data_len);
        ZlibDecoder::new(payload)
            .take(data_len as u64)
            .read_to_end(&mut out)
            .map_err(|e| ProtocolError::Compression(e.to_string()))?;
        if out.len() != data_len {
            return Err(ProtocolError::Compression(format!(
                "declared {data_len} bytes but got {}",
                out.len()
            )));
        }
        Ok(Some(out))
    }

    /// Wraps `payload` (packet id + body) into a frame ready for the socket,
    /// applying compression and encryption as configured.
    pub fn encode(&mut self, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(payload.len() + 10);
        match self.compression_threshold {
            None => {
                write_varint_to(&mut frame, payload.len() as i32);
                frame.extend_from_slice(payload);
            }
            Some(threshold) if payload.len() >= threshold => {
                let mut compressed = Vec::with_capacity(payload.len() / 2);
                let mut encoder = ZlibEncoder::new(&mut compressed, Compression::default());
                encoder.write_all(payload).expect("writing to a Vec cannot fail");
                encoder.finish().expect("writing to a Vec cannot fail");
                let inner_len = varint_len(payload.len() as i32) + compressed.len();
                write_varint_to(&mut frame, inner_len as i32);
                write_varint_to(&mut frame, payload.len() as i32);
                frame.extend_from_slice(&compressed);
            }
            Some(_) => {
                write_varint_to(&mut frame, (payload.len() + 1) as i32);
                frame.push(0);
                frame.extend_from_slice(payload);
            }
        }
        if let Some(enc) = &mut self.encryptor {
            enc.encrypt(&mut frame);
        }
        frame
    }
}

/// Reads a VarInt from the start of `buf` without consuming it.
/// Returns `(value, bytes_used)` or `None` if the buffer ends mid-VarInt.
fn peek_varint(buf: &[u8]) -> Result<Option<(usize, usize)>> {
    let mut value: u32 = 0;
    for i in 0..5 {
        let Some(&byte) = buf.get(i) else {
            return Ok(None);
        };
        value |= ((byte & 0x7F) as u32) << (7 * i);
        if byte & 0x80 == 0 {
            let value = value as i32;
            if value < 0 {
                return Err(ProtocolError::Invalid("negative frame length".into()));
            }
            return Ok(Some((value as usize, i + 1)));
        }
    }
    Err(ProtocolError::VarIntTooLong)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_roundtrip() {
        let mut a = Codec::new();
        let mut b = Codec::new();
        let frame = a.encode(&[0x00, 1, 2, 3]);
        b.push_received(&frame);
        assert_eq!(b.next_packet().unwrap().unwrap(), vec![0x00, 1, 2, 3]);
        assert!(b.next_packet().unwrap().is_none());
    }

    #[test]
    fn compressed_roundtrip() {
        let mut a = Codec::new();
        let mut b = Codec::new();
        a.enable_compression(16);
        b.enable_compression(16);
        let small = vec![0x01, 2, 3];
        let big = vec![0x02; 1000];
        let mut wire = a.encode(&small);
        wire.extend(a.encode(&big));
        // Deliver in two pieces to exercise partial reads.
        b.push_received(&wire[..5]);
        b.push_received(&wire[5..]);
        assert_eq!(b.next_packet().unwrap().unwrap(), small);
        assert_eq!(b.next_packet().unwrap().unwrap(), big);
    }

    #[test]
    fn encrypted_roundtrip() {
        let secret = [7u8; 16];
        let mut a = Codec::new();
        let mut b = Codec::new();
        a.enable_encryption(&secret);
        b.enable_encryption(&secret);
        let wire1 = a.encode(&[0x10, 9, 9]);
        let wire2 = a.encode(&[0x11, 8]);
        b.push_received(&wire1);
        b.push_received(&wire2);
        assert_eq!(b.next_packet().unwrap().unwrap(), vec![0x10, 9, 9]);
        assert_eq!(b.next_packet().unwrap().unwrap(), vec![0x11, 8]);
    }
}
