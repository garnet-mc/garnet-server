//! Named Binary Tag: Minecraft's structured binary format.
//!
//! Two flavours exist on the wire:
//! - "network" NBT (1.20.2+): the root tag has a type byte but no name.
//! - "named" NBT: the root tag has a type byte, a name, then the payload.
//!   Used on disk (level.dat, region files) and in a few older packets.
//!
//! Compounds keep insertion order so output stays stable and diffable.

use crate::{ProtocolError, Result};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum NbtTag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<NbtTag>),
    Compound(NbtCompound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

/// An ordered map of tag name to tag.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NbtCompound {
    entries: Vec<(String, NbtTag)>,
}

const TAG_END: u8 = 0;
const TAG_BYTE: u8 = 1;
const TAG_SHORT: u8 = 2;
const TAG_INT: u8 = 3;
const TAG_LONG: u8 = 4;
const TAG_FLOAT: u8 = 5;
const TAG_DOUBLE: u8 = 6;
const TAG_BYTE_ARRAY: u8 = 7;
const TAG_STRING: u8 = 8;
const TAG_LIST: u8 = 9;
const TAG_COMPOUND: u8 = 10;
const TAG_INT_ARRAY: u8 = 11;
const TAG_LONG_ARRAY: u8 = 12;

/// Nesting deeper than this is treated as malicious input.
const MAX_DEPTH: usize = 512;

impl NbtCompound {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, name: impl Into<String>, tag: impl Into<NbtTag>) -> &mut Self {
        let name = name.into();
        let tag = tag.into();
        if let Some(slot) = self.entries.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = tag;
        } else {
            self.entries.push((name, tag));
        }
        self
    }

    pub fn get(&self, name: &str) -> Option<&NbtTag> {
        self.entries.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut NbtTag> {
        self.entries.iter_mut().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    pub fn remove(&mut self, name: &str) -> Option<NbtTag> {
        let idx = self.entries.iter().position(|(n, _)| n == name)?;
        Some(self.entries.remove(idx).1)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &NbtTag)> {
        self.entries.iter().map(|(n, t)| (n.as_str(), t))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // Typed accessors: they accept any numeric tag the way the game's codecs do.

    pub fn get_compound(&self, name: &str) -> Option<&NbtCompound> {
        match self.get(name) {
            Some(NbtTag::Compound(c)) => Some(c),
            _ => None,
        }
    }

    pub fn get_list(&self, name: &str) -> Option<&[NbtTag]> {
        match self.get(name) {
            Some(NbtTag::List(l)) => Some(l),
            _ => None,
        }
    }

    pub fn get_str(&self, name: &str) -> Option<&str> {
        match self.get(name) {
            Some(NbtTag::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(NbtTag::as_i64)
    }

    pub fn get_i32(&self, name: &str) -> Option<i32> {
        self.get_i64(name).map(|v| v as i32)
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        self.get(name).and_then(NbtTag::as_f64)
    }

    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.get_i64(name).map(|v| v != 0)
    }

    pub fn get_long_array(&self, name: &str) -> Option<&[i64]> {
        match self.get(name) {
            Some(NbtTag::LongArray(a)) => Some(a),
            _ => None,
        }
    }
}

impl NbtTag {
    pub fn type_id(&self) -> u8 {
        match self {
            NbtTag::Byte(_) => TAG_BYTE,
            NbtTag::Short(_) => TAG_SHORT,
            NbtTag::Int(_) => TAG_INT,
            NbtTag::Long(_) => TAG_LONG,
            NbtTag::Float(_) => TAG_FLOAT,
            NbtTag::Double(_) => TAG_DOUBLE,
            NbtTag::ByteArray(_) => TAG_BYTE_ARRAY,
            NbtTag::String(_) => TAG_STRING,
            NbtTag::List(_) => TAG_LIST,
            NbtTag::Compound(_) => TAG_COMPOUND,
            NbtTag::IntArray(_) => TAG_INT_ARRAY,
            NbtTag::LongArray(_) => TAG_LONG_ARRAY,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        Some(match self {
            NbtTag::Byte(v) => *v as i64,
            NbtTag::Short(v) => *v as i64,
            NbtTag::Int(v) => *v as i64,
            NbtTag::Long(v) => *v,
            NbtTag::Float(v) => *v as i64,
            NbtTag::Double(v) => *v as i64,
            _ => return None,
        })
    }

    pub fn as_f64(&self) -> Option<f64> {
        Some(match self {
            NbtTag::Byte(v) => *v as f64,
            NbtTag::Short(v) => *v as f64,
            NbtTag::Int(v) => *v as f64,
            NbtTag::Long(v) => *v as f64,
            NbtTag::Float(v) => *v as f64,
            NbtTag::Double(v) => *v,
            _ => return None,
        })
    }

    pub fn as_compound(&self) -> Option<&NbtCompound> {
        match self {
            NbtTag::Compound(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            NbtTag::String(s) => Some(s),
            _ => None,
        }
    }

    // ---- writing ----

    /// Network form: type byte, then payload. No name.
    pub fn write_network(&self, out: &mut Vec<u8>) {
        out.push(self.type_id());
        self.write_payload(out);
    }

    /// Named form: type byte, name, payload. Used for files.
    pub fn write_named(&self, name: &str, out: &mut Vec<u8>) {
        out.push(self.type_id());
        write_nbt_string(name, out);
        self.write_payload(out);
    }

    fn write_payload(&self, out: &mut Vec<u8>) {
        match self {
            NbtTag::Byte(v) => out.push(*v as u8),
            NbtTag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
            NbtTag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            NbtTag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
            NbtTag::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
            NbtTag::Double(v) => out.extend_from_slice(&v.to_be_bytes()),
            NbtTag::ByteArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                out.extend(v.iter().map(|b| *b as u8));
            }
            NbtTag::String(s) => write_nbt_string(s, out),
            NbtTag::List(items) => {
                let elem_type = items.first().map(NbtTag::type_id).unwrap_or(TAG_END);
                out.push(elem_type);
                out.extend_from_slice(&(items.len() as i32).to_be_bytes());
                for item in items {
                    item.write_payload(out);
                }
            }
            NbtTag::Compound(c) => {
                for (name, tag) in &c.entries {
                    tag.write_named(name, out);
                }
                out.push(TAG_END);
            }
            NbtTag::IntArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for i in v {
                    out.extend_from_slice(&i.to_be_bytes());
                }
            }
            NbtTag::LongArray(v) => {
                out.extend_from_slice(&(v.len() as i32).to_be_bytes());
                for i in v {
                    out.extend_from_slice(&i.to_be_bytes());
                }
            }
        }
    }

    // ---- reading ----

    /// Reads network NBT. Returns the tag and the number of bytes consumed.
    pub fn read_network(data: &[u8]) -> Result<(NbtTag, usize)> {
        let mut cursor = Cursor { data, pos: 0 };
        let type_id = cursor.u8()?;
        if type_id == TAG_END {
            // An "empty" optional NBT value: the game sends a lone TAG_End.
            return Ok((NbtTag::Compound(NbtCompound::new()), 1));
        }
        let tag = read_payload(&mut cursor, type_id, 0)?;
        Ok((tag, cursor.pos))
    }

    /// Reads named NBT. Returns the root name, the tag and the bytes consumed.
    pub fn read_named(data: &[u8]) -> Result<(String, NbtTag, usize)> {
        let mut cursor = Cursor { data, pos: 0 };
        let type_id = cursor.u8()?;
        if type_id == TAG_END {
            return Err(ProtocolError::InvalidNbt("root is TAG_End".into()));
        }
        let name = cursor.string()?;
        let tag = read_payload(&mut cursor, type_id, 0)?;
        Ok((name, tag, cursor.pos))
    }

    // ---- JSON conversion ----

    /// Converts a JSON value (for example a datapack file) into NBT. Integers
    /// become Int or Long depending on size, other numbers become Double and
    /// booleans become Byte. This matches what the game's codecs accept.
    pub fn from_json(value: &Value) -> NbtTag {
        match value {
            Value::Null => NbtTag::Compound(NbtCompound::new()),
            Value::Bool(b) => NbtTag::Byte(*b as i8),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                        NbtTag::Int(i as i32)
                    } else {
                        NbtTag::Long(i)
                    }
                } else {
                    NbtTag::Double(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => NbtTag::String(s.clone()),
            Value::Array(items) => NbtTag::List(json_list_to_nbt(items)),
            Value::Object(map) => {
                let mut compound = NbtCompound::new();
                for (k, v) in map {
                    compound.put(k.clone(), NbtTag::from_json(v));
                }
                NbtTag::Compound(compound)
            }
        }
    }

    /// Converts NBT back into JSON. Numbers lose their exact NBT type.
    pub fn to_json(&self) -> Value {
        match self {
            NbtTag::Byte(v) => Value::from(*v),
            NbtTag::Short(v) => Value::from(*v),
            NbtTag::Int(v) => Value::from(*v),
            NbtTag::Long(v) => Value::from(*v),
            NbtTag::Float(v) => Value::from(*v),
            NbtTag::Double(v) => Value::from(*v),
            NbtTag::ByteArray(v) => Value::Array(v.iter().map(|b| Value::from(*b)).collect()),
            NbtTag::String(s) => Value::String(s.clone()),
            NbtTag::List(items) => Value::Array(items.iter().map(NbtTag::to_json).collect()),
            NbtTag::Compound(c) => {
                let mut map = serde_json::Map::new();
                for (k, v) in c.iter() {
                    map.insert(k.to_owned(), v.to_json());
                }
                Value::Object(map)
            }
            NbtTag::IntArray(v) => Value::Array(v.iter().map(|i| Value::from(*i)).collect()),
            NbtTag::LongArray(v) => Value::Array(v.iter().map(|i| Value::from(*i)).collect()),
        }
    }
}

/// NBT lists must be homogeneous. JSON arrays mixing ints and floats are
/// promoted to all-Double; anything else mixed becomes a list of compounds
/// wrapping each value, which the game's codecs never ask for, so we just
/// keep the first element's type and coerce.
fn json_list_to_nbt(items: &[Value]) -> Vec<NbtTag> {
    let tags: Vec<NbtTag> = items.iter().map(NbtTag::from_json).collect();
    if tags.is_empty() {
        return tags;
    }
    let all_numeric = tags.iter().all(|t| t.as_f64().is_some());
    let same_type = tags.iter().all(|t| t.type_id() == tags[0].type_id());
    if same_type {
        return tags;
    }
    if all_numeric {
        return tags
            .into_iter()
            .map(|t| NbtTag::Double(t.as_f64().unwrap()))
            .collect();
    }
    // Mixed non-numeric list: wrap each element so the list is homogeneous.
    tags.into_iter()
        .map(|t| {
            let mut c = NbtCompound::new();
            c.put("", t);
            NbtTag::Compound(c)
        })
        .collect()
}

fn write_nbt_string(s: &str, out: &mut Vec<u8>) {
    let bytes = to_modified_utf8(s);
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(&bytes);
}

/// Java's "modified UTF-8": NUL is written as C0 80 and characters outside the
/// BMP are written as surrogate pairs. Plain text is unaffected.
fn to_modified_utf8(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let code = ch as u32;
        if code == 0 {
            out.extend_from_slice(&[0xC0, 0x80]);
        } else if code < 0x10000 {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        } else {
            let v = code - 0x10000;
            let high = 0xD800 + (v >> 10);
            let low = 0xDC00 + (v & 0x3FF);
            for unit in [high, low] {
                out.push(0xE0 | (unit >> 12) as u8);
                out.push(0x80 | ((unit >> 6) & 0x3F) as u8);
                out.push(0x80 | (unit & 0x3F) as u8);
            }
        }
    }
    out
}

fn from_modified_utf8(bytes: &[u8]) -> String {
    // Fast path: valid UTF-8 without surrogates is the overwhelming case.
    if let Ok(s) = std::str::from_utf8(bytes) {
        if !s.contains('\u{0}') {
            return s.to_owned();
        }
    }
    let mut units: Vec<u16> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b & 0x80 == 0 {
            units.push(b as u16);
            i += 1;
        } else if b & 0xE0 == 0xC0 && i + 1 < bytes.len() {
            units.push((((b & 0x1F) as u16) << 6) | (bytes[i + 1] & 0x3F) as u16);
            i += 2;
        } else if b & 0xF0 == 0xE0 && i + 2 < bytes.len() {
            units.push(
                (((b & 0x0F) as u16) << 12)
                    | (((bytes[i + 1] & 0x3F) as u16) << 6)
                    | (bytes[i + 2] & 0x3F) as u16,
            );
            i += 3;
        } else {
            i += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.data.len() - self.pos < n {
            return Err(ProtocolError::InvalidNbt("unexpected end of data".into()));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn len(&mut self) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 {
            return Err(ProtocolError::InvalidNbt("negative length".into()));
        }
        Ok(n as usize)
    }
    fn string(&mut self) -> Result<String> {
        let len = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize;
        Ok(from_modified_utf8(self.take(len)?))
    }
}

fn read_payload(c: &mut Cursor, type_id: u8, depth: usize) -> Result<NbtTag> {
    if depth > MAX_DEPTH {
        return Err(ProtocolError::InvalidNbt("nested too deeply".into()));
    }
    Ok(match type_id {
        TAG_BYTE => NbtTag::Byte(c.u8()? as i8),
        TAG_SHORT => NbtTag::Short(c.i16()?),
        TAG_INT => NbtTag::Int(c.i32()?),
        TAG_LONG => NbtTag::Long(c.i64()?),
        TAG_FLOAT => NbtTag::Float(f32::from_bits(c.i32()? as u32)),
        TAG_DOUBLE => NbtTag::Double(f64::from_bits(c.i64()? as u64)),
        TAG_BYTE_ARRAY => {
            let len = c.len()?;
            NbtTag::ByteArray(c.take(len)?.iter().map(|b| *b as i8).collect())
        }
        TAG_STRING => NbtTag::String(c.string()?),
        TAG_LIST => {
            let elem_type = c.u8()?;
            let len = c.len()?;
            if elem_type == TAG_END && len > 0 {
                return Err(ProtocolError::InvalidNbt("list of TAG_End".into()));
            }
            let mut items = Vec::with_capacity(len.min(1024));
            for _ in 0..len {
                items.push(read_payload(c, elem_type, depth + 1)?);
            }
            NbtTag::List(items)
        }
        TAG_COMPOUND => {
            let mut compound = NbtCompound::new();
            loop {
                let t = c.u8()?;
                if t == TAG_END {
                    break;
                }
                let name = c.string()?;
                let tag = read_payload(c, t, depth + 1)?;
                compound.entries.push((name, tag));
            }
            NbtTag::Compound(compound)
        }
        TAG_INT_ARRAY => {
            let len = c.len()?;
            let mut v = Vec::with_capacity(len.min(4096));
            for _ in 0..len {
                v.push(c.i32()?);
            }
            NbtTag::IntArray(v)
        }
        TAG_LONG_ARRAY => {
            let len = c.len()?;
            let mut v = Vec::with_capacity(len.min(4096));
            for _ in 0..len {
                v.push(c.i64()?);
            }
            NbtTag::LongArray(v)
        }
        other => return Err(ProtocolError::InvalidNbt(format!("unknown tag type {other}"))),
    })
}

// Conveniences so `compound.put("x", 5)` reads naturally.
impl From<i8> for NbtTag {
    fn from(v: i8) -> Self {
        NbtTag::Byte(v)
    }
}
impl From<bool> for NbtTag {
    fn from(v: bool) -> Self {
        NbtTag::Byte(v as i8)
    }
}
impl From<i16> for NbtTag {
    fn from(v: i16) -> Self {
        NbtTag::Short(v)
    }
}
impl From<i32> for NbtTag {
    fn from(v: i32) -> Self {
        NbtTag::Int(v)
    }
}
impl From<i64> for NbtTag {
    fn from(v: i64) -> Self {
        NbtTag::Long(v)
    }
}
impl From<f32> for NbtTag {
    fn from(v: f32) -> Self {
        NbtTag::Float(v)
    }
}
impl From<f64> for NbtTag {
    fn from(v: f64) -> Self {
        NbtTag::Double(v)
    }
}
impl From<&str> for NbtTag {
    fn from(v: &str) -> Self {
        NbtTag::String(v.to_owned())
    }
}
impl From<String> for NbtTag {
    fn from(v: String) -> Self {
        NbtTag::String(v)
    }
}
impl From<NbtCompound> for NbtTag {
    fn from(v: NbtCompound) -> Self {
        NbtTag::Compound(v)
    }
}
impl From<Vec<NbtTag>> for NbtTag {
    fn from(v: Vec<NbtTag>) -> Self {
        NbtTag::List(v)
    }
}
impl From<Vec<i64>> for NbtTag {
    fn from(v: Vec<i64>) -> Self {
        NbtTag::LongArray(v)
    }
}
impl From<Vec<i32>> for NbtTag {
    fn from(v: Vec<i32>) -> Self {
        NbtTag::IntArray(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_network() {
        let mut c = NbtCompound::new();
        c.put("name", "Garnet");
        c.put("count", 42i32);
        c.put("big", 1i64 << 40);
        c.put("ratio", 0.5f64);
        c.put("list", vec![NbtTag::Int(1), NbtTag::Int(2)]);
        c.put("longs", vec![1i64, 2, 3]);
        let tag = NbtTag::Compound(c);
        let mut out = Vec::new();
        tag.write_network(&mut out);
        let (back, used) = NbtTag::read_network(&out).unwrap();
        assert_eq!(used, out.len());
        assert_eq!(back, tag);
    }

    #[test]
    fn json_conversion() {
        let json: Value = serde_json::from_str(r#"{"a":1,"b":2.5,"c":true,"d":[1,2],"e":{"f":"g"}}"#).unwrap();
        let tag = NbtTag::from_json(&json);
        let c = tag.as_compound().unwrap();
        assert_eq!(c.get("a"), Some(&NbtTag::Int(1)));
        assert_eq!(c.get("b"), Some(&NbtTag::Double(2.5)));
        assert_eq!(c.get("c"), Some(&NbtTag::Byte(1)));
        assert_eq!(c.get_compound("e").unwrap().get_str("f"), Some("g"));
    }
}
