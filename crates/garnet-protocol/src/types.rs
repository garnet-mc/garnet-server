//! Small value types shared by many packets.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// A namespaced id such as `minecraft:stone`. The namespace defaults to
/// `minecraft` when omitted, exactly like the game does.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identifier {
    pub namespace: String,
    pub path: String,
}

impl Identifier {
    pub fn new(namespace: &str, path: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            path: path.to_owned(),
        }
    }

    pub fn minecraft(path: &str) -> Self {
        Self::new("minecraft", path)
    }

    /// Accepts `namespace:path` or a bare `path`. Returns `None` when the
    /// string contains characters the game would reject.
    pub fn parse(s: &str) -> Option<Self> {
        let (namespace, path) = match s.split_once(':') {
            Some((ns, p)) => (ns, p),
            None => ("minecraft", s),
        };
        let ns_ok = namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'));
        let path_ok = path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'));
        if ns_ok && path_ok && !path.is_empty() {
            Some(Self::new(namespace, path))
        } else {
            None
        }
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.namespace, self.path)
    }
}

impl fmt::Debug for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl From<&str> for Identifier {
    fn from(s: &str) -> Self {
        Identifier::parse(s).unwrap_or_else(|| Identifier::minecraft("invalid"))
    }
}

impl Serialize for Identifier {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Identifier {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Identifier::parse(&s).ok_or_else(|| serde::de::Error::custom("invalid identifier"))
    }
}

/// An integer block position. On the wire it is packed into one i64:
/// 26 bits x, 26 bits z, 12 bits y.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub fn packed(self) -> i64 {
        ((self.x as i64 & 0x3FF_FFFF) << 38) | ((self.z as i64 & 0x3FF_FFFF) << 12) | (self.y as i64 & 0xFFF)
    }

    pub fn from_packed(v: i64) -> Self {
        // Arithmetic shifts sign-extend, which is exactly what we want.
        Self {
            x: (v >> 38) as i32,
            y: ((v << 52) >> 52) as i32,
            z: ((v << 26) >> 38) as i32,
        }
    }

    pub fn chunk(self) -> ChunkPos {
        ChunkPos::new(self.x >> 4, self.z >> 4)
    }

    pub fn offset(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }
}

/// A chunk column position (16x16 blocks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

impl ChunkPos {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    pub fn from_block(x: i32, z: i32) -> Self {
        Self::new(x >> 4, z >> 4)
    }

    /// Chebyshev distance, the metric used for view distance.
    pub fn distance_to(self, other: ChunkPos) -> i32 {
        (self.x - other.x).abs().max((self.z - other.z).abs())
    }
}

/// The six block faces, in the protocol's numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

impl Direction {
    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::Down,
            1 => Self::Up,
            2 => Self::North,
            3 => Self::South,
            4 => Self::West,
            5 => Self::East,
            _ => return None,
        })
    }

    pub fn offset(self) -> (i32, i32, i32) {
        match self {
            Self::Down => (0, -1, 0),
            Self::Up => (0, 1, 0),
            Self::North => (0, 0, -1),
            Self::South => (0, 0, 1),
            Self::West => (-1, 0, 0),
            Self::East => (1, 0, 0),
        }
    }
}

/// A signed property of a game profile. The only one Mojang uses is
/// `textures`, which carries the skin and cape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileProperty {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// Who a player is, as far as Mojang is concerned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameProfile {
    #[serde(with = "uuid::serde::simple")]
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub properties: Vec<ProfileProperty>,
}

impl GameProfile {
    /// The UUID an offline-mode server assigns: a v3 UUID of
    /// `OfflinePlayer:<name>`, the same rule vanilla uses.
    pub fn offline(name: &str) -> Self {
        Self {
            id: md5_offline(name),
            name: name.to_owned(),
            properties: Vec::new(),
        }
    }
}

/// `UUID.nameUUIDFromBytes("OfflinePlayer:" + name)` from Java: an MD5 hash with
/// the version and variant bits patched in.
fn md5_offline(name: &str) -> Uuid {
    let mut hash = md5_bytes(format!("OfflinePlayer:{name}").as_bytes());
    hash[6] = (hash[6] & 0x0f) | 0x30;
    hash[8] = (hash[8] & 0x3f) | 0x80;
    Uuid::from_bytes(hash)
}

/// A tiny self-contained MD5 so the protocol crate needs no extra dependency.
fn md5_bytes(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_pos_packing() {
        for pos in [
            BlockPos::new(0, 0, 0),
            BlockPos::new(1, 2, 3),
            BlockPos::new(-1, -64, -1),
            BlockPos::new(30_000_000, 319, -30_000_000),
        ] {
            assert_eq!(BlockPos::from_packed(pos.packed()), pos);
        }
    }

    #[test]
    fn offline_uuid_matches_vanilla() {
        // Known value for the player "Notch" on an offline server.
        let profile = GameProfile::offline("Notch");
        assert_eq!(profile.id.to_string(), "b50ad385-829d-3141-a216-7e7d7539ba7f");
    }

    #[test]
    fn identifier_parse() {
        assert_eq!(Identifier::parse("stone").unwrap().to_string(), "minecraft:stone");
        assert_eq!(Identifier::parse("garnet:voice").unwrap().namespace, "garnet");
        assert!(Identifier::parse("Bad:Case").is_none());
    }
}
