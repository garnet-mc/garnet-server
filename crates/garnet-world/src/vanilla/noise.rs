//! The noise the world is shaped out of.
//!
//! Three layers, each one the game's own: a single gradient noise over a
//! shuffled permutation table, a stack of those at doubling frequencies,
//! and the "normal" noise that pairs two stacks at a slightly odd ratio so
//! the two never line up. 26.3 works all of it out in single precision,
//! which is not what the older write-ups of this say, so the numbers here
//! are checked against the game rather than against those.

use super::rng::{PositionalSource, RandomSource, Xoroshiro};

/// The sixteen directions a gradient can point.
const GRADIENTS: [(i32, i32, i32); 16] = [
    (1, 1, 0),
    (-1, 1, 0),
    (1, -1, 0),
    (-1, -1, 0),
    (1, 0, 1),
    (-1, 0, 1),
    (1, 0, -1),
    (-1, 0, -1),
    (0, 1, 1),
    (0, -1, 1),
    (0, 1, -1),
    (0, -1, -1),
    (1, 1, 0),
    (0, -1, 1),
    (-1, 1, 0),
    (0, -1, -1),
];

/// Coordinates this far out are folded back, so the noise stays honest at
/// the edge of the world instead of falling apart.
const ROUND_OFF: f64 = 3.3554432e7;
const HALF_ROUND_OFF: f64 = 1.6777216e7;

fn wrap(x: f64) -> f64 {
    if x >= -HALF_ROUND_OFF && x < HALF_ROUND_OFF {
        x
    } else {
        x - (x / ROUND_OFF + 0.5).floor() * ROUND_OFF
    }
}

fn smoothstep(x: f32) -> f32 {
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

fn lerp(alpha: f32, from: f32, to: f32) -> f32 {
    from + alpha * (to - from)
}

fn lerp2(a1: f32, a2: f32, x00: f32, x10: f32, x01: f32, x11: f32) -> f32 {
    lerp(a2, lerp(a1, x00, x10), lerp(a1, x01, x11))
}

#[allow(clippy::too_many_arguments)]
fn lerp3(
    a1: f32,
    a2: f32,
    a3: f32,
    x000: f32,
    x100: f32,
    x010: f32,
    x110: f32,
    x001: f32,
    x101: f32,
    x011: f32,
    x111: f32,
) -> f32 {
    lerp(
        a3,
        lerp2(a1, a2, x000, x100, x010, x110),
        lerp2(a1, a2, x001, x101, x011, x111),
    )
}

/// One layer of gradient noise: a shuffled table and an offset of its own.
#[derive(Clone, Debug)]
pub struct Perlin {
    perms: [u8; 256],
    offset: (f64, f64, f64),
}

impl Perlin {
    pub fn new(random: &mut Xoroshiro) -> Self {
        let offset = (
            random.next_double() * 256.0,
            random.next_double() * 256.0,
            random.next_double() * 256.0,
        );
        let mut perms = [0u8; 256];
        for (i, slot) in perms.iter_mut().enumerate() {
            *slot = i as u8;
        }
        // A shuffle that draws from a shrinking range, as the game does.
        for i in 0..256usize {
            let offset = random.next_int_bound(256 - i as i32) as usize;
            perms.swap(i, i + offset);
        }
        Self { perms, offset }
    }

    fn permute(&self, x: i32) -> i32 {
        self.perms[(x & 0xFF) as usize] as i32
    }

    fn grad_dot(hash: i32, x: f32, y: f32, z: f32) -> f32 {
        let (gx, gy, gz) = GRADIENTS[(hash & 15) as usize];
        gx as f32 * x + gy as f32 * y + gz as f32 * z
    }

    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let x = wrap(x) + self.offset.0;
        let y = wrap(y) + self.offset.1;
        let z = wrap(z) + self.offset.2;
        let (fx, fy, fz) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let (rx, ry, rz) = ((x - fx as f64) as f32, (y - fy as f64) as f32, (z - fz as f64) as f32);

        let x0 = self.permute(fx);
        let x1 = self.permute(fx + 1);
        let xy00 = self.permute(x0 + fy);
        let xy01 = self.permute(x0 + fy + 1);
        let xy10 = self.permute(x1 + fy);
        let xy11 = self.permute(x1 + fy + 1);
        let d000 = Self::grad_dot(self.permute(xy00 + fz), rx, ry, rz);
        let d100 = Self::grad_dot(self.permute(xy10 + fz), rx - 1.0, ry, rz);
        let d010 = Self::grad_dot(self.permute(xy01 + fz), rx, ry - 1.0, rz);
        let d110 = Self::grad_dot(self.permute(xy11 + fz), rx - 1.0, ry - 1.0, rz);
        let d001 = Self::grad_dot(self.permute(xy00 + fz + 1), rx, ry, rz - 1.0);
        let d101 = Self::grad_dot(self.permute(xy10 + fz + 1), rx - 1.0, ry, rz - 1.0);
        let d011 = Self::grad_dot(self.permute(xy01 + fz + 1), rx, ry - 1.0, rz - 1.0);
        let d111 = Self::grad_dot(self.permute(xy11 + fz + 1), rx - 1.0, ry - 1.0, rz - 1.0);
        lerp3(
            smoothstep(rx),
            smoothstep(ry),
            smoothstep(rz),
            d000,
            d100,
            d010,
            d110,
            d001,
            d101,
            d011,
            d111,
        )
    }

    /// Flat sampling: the same noise read at y = 0.
    pub fn get_2d(&self, x: f64, z: f64) -> f32 {
        self.get(wrap(x), 0.0, wrap(z))
    }
}

/// One entry in a stack: a layer, how fast it runs, and how loud it is.
#[derive(Clone, Debug)]
struct Layer {
    noise: Perlin,
    frequency: f64,
    amplitude: f32,
}

/// A stack of gradient noises, added together.
#[derive(Clone, Debug, Default)]
pub struct Stack {
    layers: Vec<Layer>,
}

impl Stack {
    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let mut value = 0.0;
        for layer in &self.layers {
            let f = layer.frequency;
            value += layer.amplitude * layer.noise.get(x * f, y * f, z * f);
        }
        value
    }

    pub fn get_2d(&self, x: f64, z: f64) -> f32 {
        let mut value = 0.0;
        for layer in &self.layers {
            let f = layer.frequency;
            value += layer.amplitude * layer.noise.get_2d(x * f, z * f);
        }
        value
    }
}

/// How one octave stands relative to the others.
#[derive(Clone, Copy, Debug)]
struct Octave {
    index: i32,
    frequency: f64,
    amplitude: f64,
}

/// What a named noise in the data pack says about itself: where its
/// octaves start, how many there are, and how loud each one is.
#[derive(Clone, Debug)]
pub struct Parameters {
    pub base_octave: i32,
    pub base_amplitude: f64,
    pub octave_count: usize,
    /// One multiplier per octave; empty means every octave is at full.
    pub amplitudes: Vec<f64>,
    pub normalize: bool,
}

impl Parameters {
    /// What one of the data pack's noise files says, field for field.
    pub fn from_data(base_octave: i32, base_amplitude: f64, octave_count: usize, amplitudes: &[f64]) -> Self {
        Self {
            base_octave,
            base_amplitude,
            octave_count,
            amplitudes: amplitudes.to_vec(),
            normalize: true,
        }
    }

    /// The plain shape: so many octaves from here, all at full.
    pub fn octaves(base_octave: i32, octave_count: usize) -> Self {
        Self {
            base_octave,
            base_amplitude: 1.0,
            octave_count,
            amplitudes: Vec::new(),
            normalize: true,
        }
    }
}

fn amplitude_modifier(amplitudes: &[f64], index: usize) -> f64 {
    if amplitudes.is_empty() {
        1.0
    } else {
        amplitudes.get(index).copied().unwrap_or(0.0)
    }
}

fn build_octaves(base_octave: i32, base_amplitude: f64, octave_count: usize, normalize: bool, amplitudes: &[f64]) -> Vec<Octave> {
    let mut frequency = 2f64.powi(base_octave);
    let mut amplitude = base_amplitude;
    if normalize {
        amplitude *= 0.5f64.powi(-(octave_count as i32 - 1)) / (0.5f64.powi(-(octave_count as i32)) - 1.0);
    }
    let mut out = Vec::with_capacity(octave_count);
    for i in 0..octave_count {
        let modifier = amplitude_modifier(amplitudes, i);
        if modifier != 0.0 {
            out.push(Octave {
                index: base_octave + i as i32,
                frequency,
                amplitude: amplitude * modifier,
            });
        }
        frequency *= 2.0;
        amplitude *= 0.5;
    }
    out
}

/// A single gradient noise strays this far from the middle, on average.
const LAYER_DEVIATION: f64 = 0.2702247831245211;
const TARGET_DEVIATION: f64 = 0.3333333333333333;

fn normalization_factor(target_amplitude: f64, octaves: &[Octave]) -> f64 {
    let variance: f64 = octaves
        .iter()
        .map(|octave| {
            let deviation = LAYER_DEVIATION * octave.amplitude.abs();
            deviation * deviation
        })
        .sum();
    let input_deviation = variance.sqrt();
    if input_deviation == 0.0 {
        return 0.0;
    }
    (target_amplitude * TARGET_DEVIATION) / (input_deviation * 2f64.sqrt())
}

/// Two stacks at a hair's breadth apart in frequency, so that together
/// they never repeat. This is what everything in world generation reads.
#[derive(Clone, Debug, Default)]
pub struct Normal {
    stack: Stack,
}

/// Why the second stack runs slightly faster than the first.
const INPUT_FACTOR: f64 = 1.0181268882175227;

impl Normal {
    pub fn new(parameters: &Parameters, random: &mut Xoroshiro) -> Self {
        let octaves = build_octaves(
            parameters.base_octave,
            parameters.base_amplitude,
            parameters.octave_count,
            parameters.normalize,
            &parameters.amplitudes,
        );
        let target: f64 = octaves.iter().map(|o| o.amplitude.abs()).sum();
        let factor = normalization_factor(target, &octaves);

        let first: PositionalSource = random.fork_positional();
        let second: PositionalSource = random.fork_positional();
        let mut layers = Vec::with_capacity(octaves.len() * 2);
        for octave in &octaves {
            let name = format!("octave_{}", octave.index);
            let value_factor = (factor * octave.amplitude) as f32;
            layers.push(Layer {
                noise: Perlin::new(&mut first.from_hash_of(&name)),
                frequency: octave.frequency,
                amplitude: value_factor,
            });
            layers.push(Layer {
                noise: Perlin::new(&mut second.from_hash_of(&name)),
                frequency: octave.frequency * INPUT_FACTOR,
                amplitude: value_factor,
            });
        }
        Self { stack: Stack { layers } }
    }

    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        self.stack.get(x, y, z)
    }

    pub fn get_2d(&self, x: f64, z: f64) -> f32 {
        self.stack.get_2d(x, z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: i64 = 1234567890123;

    /// The six noises the world's climate is read from, with the numbers
    /// their own data pack files give them. Everything about where a biome
    /// sits comes out of these.
    #[test]
    fn climate_noises_match_the_game() {
        let specs: &[(&str, i32, f64, usize, &[f64], [f32; 3])] = &[
            (
                "temperature",
                -10,
                1.2453007926713473,
                6,
                &[1.5, 0.0, 1.0, 0.0, 0.0, 0.0],
                [0.5736162, 0.64334345, 0.67466414],
            ),
            (
                "vegetation",
                -8,
                0.9494731054427978,
                6,
                &[1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
                [-0.09813593, 0.12832946, -0.5544989],
            ),
            (
                "continentalness",
                -9,
                0.8880832896205223,
                9,
                &[1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0],
                [0.22739317, 0.35397905, 0.007315924],
            ),
            (
                "erosion",
                -9,
                1.063180125160734,
                5,
                &[1.0, 1.0, 0.0, 1.0, 1.0],
                [0.48497552, -0.3481862, 0.44877338],
            ),
            (
                "ridge",
                -7,
                0.9147152149950137,
                6,
                &[1.0, 2.0, 1.0, 0.0, 0.0, 0.0],
                [-0.06924285, -0.16844085, -0.09022917],
            ),
            (
                "offset",
                -3,
                0.9381732587751005,
                4,
                &[1.0, 1.0, 1.0, 0.0],
                [0.1967687, -0.21096161, -0.021368742],
            ),
        ];
        let points = [(0.0, 0.0, 0.0), (1000.5, 0.0, -2000.25), (-37.0, 64.0, 91.0)];
        for (name, base_octave, base_amplitude, octave_count, amplitudes, expected) in specs {
            let forker = Xoroshiro::from_seed(SEED).fork_positional();
            let mut random = forker.from_hash_of(&format!("minecraft:{name}"));
            let parameters = Parameters::from_data(*base_octave, *base_amplitude, *octave_count, amplitudes);
            let noise = Normal::new(&parameters, &mut random);
            for (point, want) in points.iter().zip(expected.iter()) {
                let got = noise.get(point.0, point.1, point.2);
                assert!((got - want).abs() < 1e-6, "{name} at {point:?}: {got} vs {want}");
            }
        }
    }

    /// Numbers printed by the game itself; see `scratchpad/oracle`.
    #[test]
    fn normal_noise_matches_the_game() {
        let mut random = Xoroshiro::from_seed(SEED);
        let noise = Normal::new(&Parameters::octaves(-7, 2), &mut random);
        let samples = [
            ((0.0, 0.0, 0.0), -0.19522786f32),
            ((512.5, 12.0, -128.25), -0.46291265),
            ((-2048.5, 63.0, 777.75), -0.1570552),
        ];
        for ((x, y, z), expected) in samples {
            let got = noise.get(x, y, z);
            assert!((got - expected).abs() < 1e-6, "at {x},{y},{z}: {got} vs {expected}");
        }
    }
}
