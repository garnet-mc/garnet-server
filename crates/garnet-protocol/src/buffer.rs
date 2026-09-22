//! Reading and writing the primitive types the protocol is built from.
//!
//! Everything is big-endian. Variable-length integers use the LEB128-like
//! encoding described on the Minecraft wiki: seven data bits per byte and the
//! high bit set while more bytes follow.

use crate::nbt::NbtTag;
use crate::text::Text;
use crate::types::{BlockPos, Identifier};
use crate::{ProtocolError, Result};
use uuid::Uuid;

/// Maximum length of a protocol string in characters (the vanilla default).
pub const DEFAULT_MAX_STRING: usize = 32767;

/// Builds packet bodies. Wraps a `Vec<u8>` and only ever appends.
#[derive(Default, Debug, Clone)]
pub struct PacketWriter {
    buf: Vec<u8>,
}

impl PacketWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn write_bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }

    pub fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn write_i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    pub fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    pub fn write_varint(&mut self, v: i32) {
        write_varint_to(&mut self.buf, v);
    }

    pub fn write_varlong(&mut self, v: i64) {
        let mut value = v as u64;
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    /// A string is a VarInt byte length followed by UTF-8 bytes.
    pub fn write_string(&mut self, s: &str) {
        self.write_varint(s.len() as i32);
        self.buf.extend_from_slice(s.as_bytes());
    }

    pub fn write_identifier(&mut self, id: &Identifier) {
        self.write_string(&id.to_string());
    }

    pub fn write_uuid(&mut self, uuid: &Uuid) {
        self.buf.extend_from_slice(uuid.as_bytes());
    }

    /// A byte array is a VarInt length followed by the bytes.
    pub fn write_byte_array(&mut self, bytes: &[u8]) {
        self.write_varint(bytes.len() as i32);
        self.buf.extend_from_slice(bytes);
    }

    pub fn write_block_pos(&mut self, pos: BlockPos) {
        self.write_i64(pos.packed());
    }

    /// Writes a "prefixed optional": a boolean followed by the value when present.
    pub fn write_option<T>(&mut self, value: Option<&T>, mut write: impl FnMut(&mut Self, &T)) {
        match value {
            Some(v) => {
                self.write_bool(true);
                write(self, v);
            }
            None => self.write_bool(false),
        }
    }

    /// Writes a VarInt count followed by each item.
    pub fn write_list<T>(&mut self, items: &[T], mut write: impl FnMut(&mut Self, &T)) {
        self.write_varint(items.len() as i32);
        for item in items {
            write(self, item);
        }
    }

    /// Writes a network NBT tag (type id + payload, no root name).
    pub fn write_nbt(&mut self, tag: &NbtTag) {
        tag.write_network(&mut self.buf);
    }

    /// Text components travel as NBT since 1.20.3.
    pub fn write_text(&mut self, text: &Text) {
        self.write_nbt(&text.to_nbt());
    }

    /// A bit set is sent the way `java.util.BitSet.toByteArray()` lays it
    /// out: a VarInt length and little-endian bytes, trailing zero bytes
    /// dropped. (Older versions sent longs; 26.x sends bytes.)
    pub fn write_bitset(&mut self, bits: &[u64]) {
        let mut bytes: Vec<u8> = bits.iter().flat_map(|w| w.to_le_bytes()).collect();
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        self.write_byte_array(&bytes);
    }

    /// Angle in steps of 1/256 of a full turn.
    pub fn write_angle(&mut self, degrees: f32) {
        self.write_u8(((degrees.rem_euclid(360.0) / 360.0) * 256.0) as u8);
    }
}

/// Appends a VarInt to any byte vector. Used by the framing layer too.
pub fn write_varint_to(buf: &mut Vec<u8>, v: i32) {
    let mut value = v as u32;
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

/// How many bytes `v` takes as a VarInt.
pub fn varint_len(v: i32) -> usize {
    let v = v as u32;
    match v {
        0..=0x7F => 1,
        0x80..=0x3FFF => 2,
        0x4000..=0x1F_FFFF => 3,
        0x20_0000..=0xFFF_FFFF => 4,
        _ => 5,
    }
}

/// Reads a packet body. Borrows the bytes and keeps a cursor.
pub struct PacketReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PacketReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn remaining_bytes(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(ProtocolError::UnexpectedEof {
                needed: n - self.remaining(),
            });
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    pub fn read_bool(&mut self) -> Result<bool> {
        Ok(self.take(1)?[0] != 0)
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.take(1)?[0] as i8)
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn read_i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn read_i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn read_varint(&mut self) -> Result<i32> {
        let mut value: u32 = 0;
        for i in 0..5 {
            let byte = self.read_u8()?;
            // The fifth byte only has room for 4 data bits; anything above
            // that would silently be dropped, so treat it as malformed.
            if i == 4 && byte & 0xF0 != 0 {
                return Err(ProtocolError::VarIntTooLong);
            }
            value |= ((byte & 0x7F) as u32) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(ProtocolError::VarIntTooLong)
    }

    pub fn read_varlong(&mut self) -> Result<i64> {
        let mut value: u64 = 0;
        for i in 0..10 {
            let byte = self.read_u8()?;
            if i == 9 && byte & 0xFE != 0 {
                return Err(ProtocolError::VarIntTooLong);
            }
            value |= ((byte & 0x7F) as u64) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value as i64);
            }
        }
        Err(ProtocolError::VarIntTooLong)
    }

    pub fn read_string(&mut self) -> Result<String> {
        self.read_string_max(DEFAULT_MAX_STRING)
    }

    /// `max` is measured in UTF-16 code units by the vanilla client; we count
    /// characters instead, which is stricter and therefore still safe.
    pub fn read_string_max(&mut self, max: usize) -> Result<String> {
        let len = self.read_varint()?;
        if len < 0 || len as usize > max * 4 {
            return Err(ProtocolError::StringTooLong {
                len: len.max(0) as usize,
                max,
            });
        }
        let bytes = self.take(len as usize)?;
        let s = std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidUtf8)?;
        if s.chars().count() > max {
            return Err(ProtocolError::StringTooLong {
                len: s.chars().count(),
                max,
            });
        }
        Ok(s.to_owned())
    }

    pub fn read_identifier(&mut self) -> Result<Identifier> {
        let s = self.read_string()?;
        Identifier::parse(&s).ok_or_else(|| ProtocolError::Invalid(format!("bad identifier {s}")))
    }

    pub fn read_uuid(&mut self) -> Result<Uuid> {
        Ok(Uuid::from_slice(self.take(16)?).unwrap())
    }

    pub fn read_byte_array(&mut self) -> Result<&'a [u8]> {
        let len = self.read_varint()?;
        if len < 0 {
            return Err(ProtocolError::Invalid("negative array length".into()));
        }
        self.take(len as usize)
    }

    pub fn read_block_pos(&mut self) -> Result<BlockPos> {
        Ok(BlockPos::from_packed(self.read_i64()?))
    }

    pub fn read_option<T>(&mut self, read: impl FnOnce(&mut Self) -> Result<T>) -> Result<Option<T>> {
        if self.read_bool()? {
            Ok(Some(read(self)?))
        } else {
            Ok(None)
        }
    }

    pub fn read_list<T>(&mut self, mut read: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let count = self.read_varint()?;
        if count < 0 {
            return Err(ProtocolError::Invalid("negative list length".into()));
        }
        // Guard against a hostile client asking us to allocate gigabytes up front.
        let mut items = Vec::with_capacity((count as usize).min(4096));
        for _ in 0..count {
            items.push(read(self)?);
        }
        Ok(items)
    }

    pub fn read_nbt(&mut self) -> Result<NbtTag> {
        let (tag, used) = NbtTag::read_network(self.remaining_bytes())?;
        self.pos += used;
        Ok(tag)
    }

    pub fn read_bitset(&mut self) -> Result<Vec<u64>> {
        let bytes = self.read_byte_array()?;
        Ok(bytes
            .chunks(8)
            .map(|c| {
                let mut word = [0u8; 8];
                word[..c.len()].copy_from_slice(c);
                u64::from_le_bytes(word)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0, 1, 127, 128, 255, 2147483647, -1, -2147483648, 25565] {
            let mut w = PacketWriter::new();
            w.write_varint(v);
            assert_eq!(w.len(), varint_len(v));
            let mut r = PacketReader::new(w.as_slice());
            assert_eq!(r.read_varint().unwrap(), v);
        }
    }

    #[test]
    fn bitset_matches_java_layout() {
        // 26 sections + 2 = bits 0..=25 set: BitSet.toByteArray() gives
        // ff ff ff 03, so the wire form is 04 ff ff ff 03.
        let mut w = PacketWriter::new();
        w.write_bitset(&[0x03ff_ffff]);
        assert_eq!(w.as_slice(), &[4, 0xff, 0xff, 0xff, 0x03]);
        let mut r = PacketReader::new(w.as_slice());
        assert_eq!(r.read_bitset().unwrap(), vec![0x03ff_ffff]);

        let mut w = PacketWriter::new();
        w.write_bitset(&[]);
        assert_eq!(w.as_slice(), &[0]);
    }

    #[test]
    fn varint_known_encodings() {
        let mut w = PacketWriter::new();
        w.write_varint(300);
        assert_eq!(w.as_slice(), &[0xAC, 0x02]);
        let mut w = PacketWriter::new();
        w.write_varint(-1);
        assert_eq!(w.as_slice(), &[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
    }

    #[test]
    fn varint_rejects_overflow_bits() {
        // Five bytes whose last byte carries bits that do not fit in 32 bits.
        let mut r = PacketReader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0x7F]);
        assert!(r.read_varint().is_err());
    }

    #[test]
    fn string_roundtrip() {
        let mut w = PacketWriter::new();
        w.write_string("héllo wörld");
        let mut r = PacketReader::new(w.as_slice());
        assert_eq!(r.read_string().unwrap(), "héllo wörld");
        assert!(r.is_empty());
    }
}
