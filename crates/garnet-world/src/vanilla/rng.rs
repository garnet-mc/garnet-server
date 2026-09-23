//! The game's own random sources, bit for bit.
//!
//! Everything about a vanilla world -- where the land is, which biome sits
//! where, which block the surface is made of, where a village stands --
//! comes out of these two generators and the way they are seeded. Nothing
//! downstream can match vanilla unless these match first, so each one is
//! checked against numbers taken from the game itself: see the tests at the
//! foot of the file and `scratchpad/oracle`, the little Java program that
//! prints them.

/// A source of random numbers, in either of the shapes the game uses.
pub trait RandomSource {
    fn next_bits(&mut self, bits: u32) -> i32;
    fn next_long(&mut self) -> i64;

    fn next_int(&mut self) -> i32 {
        self.next_bits(32)
    }

    /// The old way of drawing under a bound: take the top bits and take
    /// the remainder, trying again when the draw fell in the ragged tail.
    fn next_int_bound(&mut self, bound: i32) -> i32 {
        if bound <= 0 {
            return 0;
        }
        if bound & (bound - 1) == 0 {
            return ((bound as i64).wrapping_mul(self.next_bits(31) as i64) >> 31) as i32;
        }
        loop {
            let bits = self.next_bits(31);
            let value = bits % bound;
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }

    fn next_double(&mut self) -> f64 {
        let high = (self.next_bits(26) as i64) << 27;
        let low = self.next_bits(27) as i64;
        (high + low) as f64 * 1.110_223_024_625_156_5e-16
    }

    fn next_float(&mut self) -> f32 {
        self.next_bits(24) as f32 * 5.9604645e-8
    }
}

/// xoroshiro128++, which is what the game uses for everything new.
#[derive(Clone, Debug)]
pub struct Xoroshiro {
    lo: u64,
    hi: u64,
}

impl Xoroshiro {
    /// From one seed, spread over 128 bits the way the game spreads it.
    pub fn from_seed(seed: i64) -> Self {
        let (lo, hi) = upgrade_seed(seed);
        Self::from_parts(lo, hi)
    }

    pub fn from_parts(lo: i64, hi: i64) -> Self {
        // Both halves zero would leave it stuck; the game picks these.
        let (lo, hi) = (lo as u64, hi as u64);
        if lo == 0 && hi == 0 {
            return Self {
                lo: 0x9E3779B97F4A7C15,
                hi: 0x6A09E667F3BCC909,
            };
        }
        Self { lo, hi }
    }

    fn next_u64(&mut self) -> u64 {
        let lo = self.lo;
        let mut hi = self.hi;
        let out = lo.wrapping_add(hi).rotate_left(17).wrapping_add(lo);
        hi ^= lo;
        self.lo = lo.rotate_left(49) ^ hi ^ (hi << 21);
        self.hi = hi.rotate_left(28);
        out
    }

    /// A source of forks, one for each named thing that wants its own.
    pub fn fork_positional(&mut self) -> PositionalSource {
        PositionalSource {
            lo: self.next_u64() as i64,
            hi: self.next_u64() as i64,
        }
    }
}

impl RandomSource for Xoroshiro {
    fn next_bits(&mut self, bits: u32) -> i32 {
        (self.next_u64() >> (64 - bits)) as i32
    }

    fn next_long(&mut self) -> i64 {
        self.next_u64() as i64
    }

    fn next_int(&mut self) -> i32 {
        self.next_long() as i32
    }

    /// Both of these are worked out in single precision in the game, down
    /// to the constant being written as a float, so they are worked out
    /// that way here.
    fn next_double(&mut self) -> f64 {
        ((self.next_u64() >> 11) as i64) as f64 * 1.110223e-16f32 as f64
    }

    fn next_float(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 * 5.9604645e-8
    }

    /// The newer generator draws under a bound the cheap way: multiply and
    /// take the top half, and only fall back to trying again for the few
    /// draws that would skew the spread.
    fn next_int_bound(&mut self, bound: i32) -> i32 {
        if bound <= 0 {
            return 0;
        }
        let bound = bound as i64;
        let mut bits = self.next_int() as u32 as i64;
        let mut multiplied = bits.wrapping_mul(bound);
        let mut fraction = multiplied & 0xFFFF_FFFF;
        if fraction < bound {
            let threshold = ((!bound as u32).wrapping_add(1) as u32 % bound as u32) as i64;
            while fraction < threshold {
                bits = self.next_int() as u32 as i64;
                multiplied = bits.wrapping_mul(bound);
                fraction = multiplied & 0xFFFF_FFFF;
            }
        }
        (multiplied >> 32) as i32
    }
}

/// `java.util.Random`, which the older parts of the game still use.
#[derive(Clone, Debug)]
pub struct Legacy {
    seed: i64,
}

impl Legacy {
    const MULTIPLIER: i64 = 0x5DEECE66D;
    const MASK: i64 = (1 << 48) - 1;

    pub fn from_seed(seed: i64) -> Self {
        Self {
            seed: (seed ^ Self::MULTIPLIER) & Self::MASK,
        }
    }
}

impl RandomSource for Legacy {
    fn next_bits(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(Self::MULTIPLIER).wrapping_add(0xB) & Self::MASK;
        (self.seed >> (48 - bits)) as i32
    }

    fn next_long(&mut self) -> i64 {
        let high = (self.next_bits(32) as i64) << 32;
        high.wrapping_add(self.next_bits(32) as i64)
    }
}

/// What hands out a random source per name or per position, so that two
/// worlds with the same seed put the same noise in the same place however
/// many other things were generated in between.
#[derive(Clone, Debug)]
pub struct PositionalSource {
    lo: i64,
    hi: i64,
}

impl PositionalSource {
    /// One for a named thing, such as `minecraft:temperature`.
    pub fn from_hash_of(&self, name: &str) -> Xoroshiro {
        let digest = md5(name.as_bytes());
        let lo = i64::from_be_bytes(digest[0..8].try_into().unwrap());
        let hi = i64::from_be_bytes(digest[8..16].try_into().unwrap());
        Xoroshiro::from_parts(lo ^ self.lo, hi ^ self.hi)
    }

    /// One for a place in the world.
    pub fn at(&self, x: i32, y: i32, z: i32) -> Xoroshiro {
        let mixed = block_seed(x, y, z);
        Xoroshiro::from_parts(mixed ^ self.lo, self.hi)
    }
}

/// The game's own seed scrambling: one long into two.
fn upgrade_seed(seed: i64) -> (i64, i64) {
    const GOLDEN: i64 = -0x61c8864680b583ebi64; // 0x9E3779B97F4A7C15
    let lo = seed ^ 0x6A09E667F3BCC909u64 as i64;
    let hi = lo.wrapping_add(GOLDEN);
    (mix_stafford13(lo), mix_stafford13(hi))
}

/// Stafford's variant 13, the mixer the game uses on its seeds.
fn mix_stafford13(value: i64) -> i64 {
    let mut v = value as u64;
    v = (v ^ (v >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94D049BB133111EB);
    (v ^ (v >> 31)) as i64
}

/// How a block position becomes a seed.
fn block_seed(x: i32, y: i32, z: i32) -> i64 {
    let seed = (x as i64).wrapping_mul(3129871) ^ (z as i64).wrapping_mul(116129781) ^ (y as i64);
    let seed = seed
        .wrapping_mul(seed)
        .wrapping_mul(42317861)
        .wrapping_add(seed.wrapping_mul(11));
    seed >> 16
}

/// MD5, because that is what the game hashes a noise's name with. Small
/// and self-contained: pulling in a crate for sixteen bytes would be
/// heavier than the thing itself.
fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4,
        11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: [u32; 64] = std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32);

    let mut message = input.to_vec();
    let bit_length = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in message.chunks(64) {
        let words: [u32; 16] = std::array::from_fn(|i| u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap()));
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(words[g]);
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

    /// Every number here came out of the game itself, printed by the Java
    /// program in `scratchpad/oracle` against Minecraft 26.3.
    const SEED: i64 = 1234567890123;

    #[test]
    fn md5_matches_the_standard() {
        // The empty string and "abc", which every MD5 agrees on.
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn xoroshiro_matches_the_game() {
        let mut r = Xoroshiro::from_seed(SEED);
        assert_eq!(
            (0..6).map(|_| r.next_long()).collect::<Vec<_>>(),
            vec![
                -2624490476143626894,
                -4120571446321590704,
                -8577917762645600131,
                -9031676666403446600,
                3319664545208770767,
                -5757480376081603304,
            ]
        );
    }

    #[test]
    fn xoroshiro_ints_and_floats_match() {
        let mut r = Xoroshiro::from_seed(SEED);
        assert_eq!(
            (0..4).map(|_| r.next_int_bound(100)).collect::<Vec<_>>(),
            vec![17, 93, 11, 48]
        );
        let doubles: Vec<f64> = (0..3).map(|_| r.next_double()).collect();
        assert!((doubles[0] - 0.17995937559192265).abs() < 1e-15);
        assert!((doubles[1] - 0.6878863634104833).abs() < 1e-15);
        assert!((doubles[2] - 0.6310010330400626).abs() < 1e-15);
        let floats: Vec<f32> = (0..3).map(|_| r.next_float()).collect();
        assert!((floats[0] - 0.07107526).abs() < 1e-7);
        assert!((floats[1] - 0.27638096).abs() < 1e-7);
        assert!((floats[2] - 0.1375069).abs() < 1e-7);
    }

    #[test]
    fn legacy_matches_the_game() {
        let mut r = Legacy::from_seed(SEED);
        assert_eq!(
            (0..4).map(|_| r.next_long()).collect::<Vec<_>>(),
            vec![
                -37462751138084332,
                -4294232599635685378,
                -3937534964849336365,
                -4427497540126799758,
            ]
        );
    }

    #[test]
    fn named_forks_match_the_game() {
        let forker = Xoroshiro::from_seed(SEED).fork_positional();
        let expected: &[(&str, i64, i64)] = &[
            ("minecraft:temperature", -2602265052503692807, 975129240295159894),
            ("minecraft:vegetation", -7107018248851339650, -5659505876910163963),
            ("minecraft:continentalness", 4914479926923433524, -4090933504303544621),
            ("minecraft:erosion", 4508319539931673758, -7386526379862668958),
            ("minecraft:ridge", 8640858984896545144, -2658102868207992538),
            ("minecraft:offset", 2631823185994711550, 1804666954599533888),
        ];
        for (name, first, second) in expected {
            let mut forked = forker.from_hash_of(name);
            assert_eq!(forked.next_long(), *first, "{name} first");
            assert_eq!(forked.next_long(), *second, "{name} second");
        }
    }

    #[test]
    fn positional_forks_match_the_game() {
        let forker = Xoroshiro::from_seed(SEED).fork_positional();
        let mut at = forker.at(12, 70, -345);
        assert_eq!(
            (0..3).map(|_| at.next_long()).collect::<Vec<_>>(),
            vec![1284008107458964419, -133117026469737108, 8298150425817638970]
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
