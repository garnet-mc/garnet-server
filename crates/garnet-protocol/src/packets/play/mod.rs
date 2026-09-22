//! Play state: everything that happens once the player is in the world.

pub mod clientbound;
pub mod serverbound;


use crate::buffer::PacketWriter;

/// Game modes as the protocol numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameMode {
    Survival = 0,
    Creative = 1,
    Adventure = 2,
    Spectator = 3,
}

impl GameMode {
    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::Survival,
            1 => Self::Creative,
            2 => Self::Adventure,
            3 => Self::Spectator,
            _ => return None,
        })
    }

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "survival" | "s" | "0" => Self::Survival,
            "creative" | "c" | "1" => Self::Creative,
            "adventure" | "a" | "2" => Self::Adventure,
            "spectator" | "sp" | "3" => Self::Spectator,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Survival => "survival",
            Self::Creative => "creative",
            Self::Adventure => "adventure",
            Self::Spectator => "spectator",
        }
    }
}

/// A velocity packed into Mojang's "LpVec3" format (26.1+). Each component
/// is quantised to 15 bits relative to a shared scale so small velocities
/// stay precise and zero velocity is a single byte.
pub fn write_lp_vec3(w: &mut PacketWriter, x: f64, y: f64, z: f64) {
    const LIMIT: f64 = 1.717_986_918_3e10;
    let clamp = |v: f64| if v.is_nan() { 0.0 } else { v.clamp(-LIMIT, LIMIT) };
    let (x, y, z) = (clamp(x), clamp(y), clamp(z));
    let max = x.abs().max(y.abs()).max(z.abs());
    if max < 3.051_944_088_384_301e-5 {
        w.write_u8(0);
        return;
    }
    let scale = max.ceil() as i64;
    let extended = scale > 3;
    let header = if extended { (scale & 3) | 4 } else { scale };
    // Map -1..1 onto 0..32766 in 15 bits.
    let quantise = |v: f64| (((v / scale as f64) * 0.5 + 0.5) * 32766.0).round().clamp(0.0, 32766.0) as i64;
    let packed: i64 = header | (quantise(x) << 3) | (quantise(y) << 18) | (quantise(z) << 33);
    // The low 16 bits are little-endian, the next 32 big-endian. Yes, really.
    w.write_bytes(&(packed as u16).to_le_bytes());
    w.write_i32((packed >> 16) as i32);
    if extended {
        w.write_varint((scale >> 2) as i32);
    }
}
