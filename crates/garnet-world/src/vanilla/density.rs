//! The graph of functions a vanilla world is worked out from.
//!
//! The data pack writes world generation as a graph: every value the game
//! wants at a point -- how warm it is, how far from the sea, how solid the
//! rock -- is a small tree of nodes over the noises. This reads those trees
//! and works them out at a point, which is all the climate needs. Terrain
//! wants the same graph read over a whole volume at once, which will come
//! later; the shapes here are the ones the game uses either way.

use super::noise::lerp3;
use super::noise::{Blended, Normal, Parameters};
use super::rng::Xoroshiro;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Where the game keeps the noises and the functions that read them.
pub struct Graph {
    functions: HashMap<String, Arc<Node>>,
    /// The random source the old terrain noise draws its octaves from.
    terrain_random: Xoroshiro,
}

/// One node of a density function.
#[derive(Debug)]
pub enum Node {
    Constant(f32),
    /// A noise read at a point, with the sampling scaled and shifted.
    Noise {
        noise: Arc<Normal>,
        xz_scale: f64,
        y_scale: f64,
        shift_x: Arc<Node>,
        shift_y: Arc<Node>,
        shift_z: Arc<Node>,
    },
    /// The two halves of the offset that bends the climate about, and the
    /// three-dimensional form of the same.
    ShiftA(Arc<Normal>),
    ShiftB(Arc<Normal>),
    Shift(Arc<Normal>),
    /// A straight line along one axis between two heights.
    Gradient {
        axis: Axis,
        from_coordinate: i32,
        to_coordinate: i32,
        from_value: f32,
        to_value: f32,
    },
    Add(Arc<Node>, Arc<Node>),
    Sub(Arc<Node>, Arc<Node>),
    Mul(Arc<Node>, Arc<Node>),
    Div(Arc<Node>, Arc<Node>),
    Min(Arc<Node>, Arc<Node>),
    Max(Arc<Node>, Arc<Node>),
    Negate(Arc<Node>),
    Abs(Arc<Node>),
    Square(Arc<Node>),
    Cube(Arc<Node>),
    /// Halves or quarters whatever is below zero, leaving the rest alone.
    HalfNegative(Arc<Node>),
    QuarterNegative(Arc<Node>),
    /// Pulls the middle of the range towards nothing.
    Squeeze(Arc<Node>),
    Clamp {
        input: Arc<Node>,
        min: f32,
        max: f32,
    },
    Lerp {
        alpha: Arc<Node>,
        first: Arc<Node>,
        second: Arc<Node>,
    },
    Spline {
        coordinate: Arc<Node>,
        points: Arc<Spline>,
    },
    RangeChoice {
        input: Arc<Node>,
        min: f32,
        max: f32,
        in_range: Arc<Node>,
        out_of_range: Arc<Node>,
    },
    /// The older terrain noise, which the overworld is still shaped by.
    OldBlended(Arc<Blended>),
    /// A value taken at the corners of a cell and read between, which is
    /// what keeps the terrain smooth between the points it is worked out
    /// at.
    Interpolated {
        input: Arc<Node>,
        cell_xz: i32,
        cell_y: i32,
    },
    /// Picks one of several by which band the input falls in: the first
    /// whose threshold the input is under, or the last.
    IntervalSelect {
        input: Arc<Node>,
        thresholds: Vec<f32>,
        choices: Vec<Arc<Node>>,
    },
    /// Blending with an older world, of which there is none here: the
    /// alpha is all the way over and the offset is nothing.
    BlendAlpha,
    BlendOffset,
    /// A function that stands for another: a name that was looked up, or
    /// a cache, which changes nothing about the value.
    Reference(Arc<Node>),
    /// Anything we have not taught it yet reads as nothing at all.
    Unsupported(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

/// A curve given as points with slopes, read between with a cubic.
#[derive(Debug)]
pub struct Spline {
    locations: Vec<f32>,
    derivatives: Vec<f32>,
    values: Vec<SplineValue>,
}

#[derive(Debug)]
enum SplineValue {
    Fixed(f32),
    Nested { coordinate: Arc<Node>, points: Arc<Spline> },
}

impl Graph {
    /// Reads every density function and noise in the data pack, and builds
    /// the noises for one world seed.
    pub fn load(datapack: &Path, seed: i64) -> Self {
        let noises = load_noises(datapack, seed);
        let root = datapack.join("minecraft").join("worldgen").join("density_function");
        let mut files = HashMap::new();
        collect_files(&root, &root, &mut files);
        let mut graph = Self {
            functions: HashMap::new(),
            terrain_random: Xoroshiro::from_seed(seed).fork_positional().from_hash_of("minecraft:terrain"),
        };
        let names: Vec<String> = files.keys().cloned().collect();
        for name in names {
            graph.build(&name, &files, &noises, 0);
        }
        tracing::info!("read {} density functions", graph.functions.len());
        graph
    }

    /// One function by name, such as `minecraft:overworld/temperature`.
    pub fn function(&self, name: &str) -> Option<Arc<Node>> {
        self.functions.get(name).cloned()
    }

    fn build(
        &mut self,
        name: &str,
        files: &HashMap<String, Value>,
        noises: &HashMap<String, Arc<Normal>>,
        depth: usize,
    ) -> Arc<Node> {
        if let Some(found) = self.functions.get(name) {
            return found.clone();
        }
        if depth > 32 {
            return Arc::new(Node::Unsupported(format!("{name} refers to itself")));
        }
        let Some(json) = files.get(name) else {
            return Arc::new(Node::Unsupported(name.to_owned()));
        };
        let node = Arc::new(self.parse(json, files, noises, depth));
        self.functions.insert(name.to_owned(), node.clone());
        node
    }

    fn parse(
        &mut self,
        json: &Value,
        files: &HashMap<String, Value>,
        noises: &HashMap<String, Arc<Normal>>,
        depth: usize,
    ) -> Node {
        match json {
            Value::Number(number) => return Node::Constant(number.as_f64().unwrap_or(0.0) as f32),
            // A name stands for the function it names; sharing the node
            // keeps the graph a graph rather than a forest.
            Value::String(name) => return Node::Reference(self.build(&with_namespace(name), files, noises, depth + 1)),
            _ => {}
        }
        let kind = json.get("type").and_then(Value::as_str).unwrap_or_default();
        let child = |graph: &mut Self, key: &str| -> Arc<Node> {
            match json.get(key) {
                Some(value) => Arc::new(graph.parse(value, files, noises, depth + 1)),
                None => Arc::new(Node::Constant(0.0)),
            }
        };
        let number = |key: &str, fallback: f64| json.get(key).and_then(Value::as_f64).unwrap_or(fallback);
        let noise_named = |key: &str| -> Arc<Normal> {
            let name = json.get(key).and_then(Value::as_str).map(with_namespace).unwrap_or_default();
            noises.get(&name).cloned().unwrap_or_default()
        };

        match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "constant" => Node::Constant(number("argument", 0.0) as f32),
            "noise" => Node::Noise {
                noise: noise_named("noise"),
                xz_scale: number("xz_scale", 1.0),
                y_scale: number("y_scale", 1.0),
                shift_x: child(self, "shift_x"),
                shift_y: child(self, "shift_y"),
                shift_z: child(self, "shift_z"),
            },
            "shift_a" => Node::ShiftA(noise_named("noise")),
            "shift_b" => Node::ShiftB(noise_named("noise")),
            "shift" => Node::Shift(noise_named("noise")),
            "gradient" => Node::Gradient {
                axis: match json.get("axis").and_then(Value::as_str) {
                    Some("x") => Axis::X,
                    Some("z") => Axis::Z,
                    _ => Axis::Y,
                },
                from_coordinate: number("from_coordinate", 0.0) as i32,
                to_coordinate: number("to_coordinate", 0.0) as i32,
                from_value: number("from_value", 0.0) as f32,
                to_value: number("to_value", 0.0) as f32,
            },
            "add" => Node::Add(child(self, "left"), child(self, "right")),
            "sub" => Node::Sub(child(self, "left"), child(self, "right")),
            "mul" => Node::Mul(child(self, "left"), child(self, "right")),
            "div" => Node::Div(child(self, "left"), child(self, "right")),
            "min" => Node::Min(child(self, "left"), child(self, "right")),
            "max" => Node::Max(child(self, "left"), child(self, "right")),
            "negate" => Node::Negate(child(self, "input")),
            "abs" => Node::Abs(child(self, "input")),
            "square" => Node::Square(child(self, "input")),
            "cube" => Node::Cube(child(self, "input")),
            "half_negative" => Node::HalfNegative(child(self, "input")),
            "quarter_negative" => Node::QuarterNegative(child(self, "input")),
            "squeeze" => Node::Squeeze(child(self, "input")),
            "clamp" => Node::Clamp {
                input: child(self, "input"),
                min: number("min", f64::from(f32::MIN)) as f32,
                max: number("max", f64::from(f32::MAX)) as f32,
            },
            "lerp" => Node::Lerp {
                alpha: child(self, "alpha"),
                first: child(self, "first"),
                second: child(self, "second"),
            },
            "range_choice" => Node::RangeChoice {
                input: child(self, "input"),
                min: number("min_inclusive", 0.0) as f32,
                max: number("max_exclusive", 0.0) as f32,
                in_range: child(self, "when_in_range"),
                out_of_range: child(self, "when_out_of_range"),
            },
            "spline" => {
                let spline = json.get("spline");
                match spline {
                    Some(Value::Object(_)) => {
                        let (coordinate, points) = self.parse_spline(spline.unwrap(), files, noises, depth + 1);
                        Node::Spline { coordinate, points }
                    }
                    Some(Value::Number(n)) => Node::Constant(n.as_f64().unwrap_or(0.0) as f32),
                    _ => Node::Constant(0.0),
                }
            }
            // Cached values are the same values; the caching is only there
            // to save the game work.
            "cache" | "flat_cache" | "cache_2d" | "cache_once" | "cache_all_in_cell" | "blend_density" => {
                Node::Reference(child(self, "input"))
            }
            "old_blended_noise" => {
                // One random source of its own, named for the terrain.
                let mut random = self.terrain_random.clone();
                Node::OldBlended(Arc::new(Blended::new(
                    &mut random,
                    number("xz_scale", 1.0),
                    number("y_scale", 1.0),
                    number("xz_factor", 80.0),
                    number("y_factor", 160.0),
                    number("smear_scale_multiplier", 8.0),
                )))
            }
            "interpolated" => Node::Interpolated {
                input: child(self, "input"),
                cell_xz: number("cell_size_xz", 4.0) as i32,
                cell_y: number("cell_size_y", 8.0) as i32,
            },
            "interval_select" => {
                let thresholds: Vec<f32> = json
                    .get("thresholds")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().filter_map(Value::as_f64).map(|v| v as f32).collect())
                    .unwrap_or_default();
                let choices: Vec<Arc<Node>> = json
                    .get("functions")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .map(|value| Arc::new(self.parse(value, files, noises, depth + 1)))
                            .collect()
                    })
                    .unwrap_or_default();
                Node::IntervalSelect {
                    input: child(self, "input"),
                    thresholds,
                    choices,
                }
            }
            "blend_alpha" => Node::BlendAlpha,
            "blend_offset" => Node::BlendOffset,
            other => Node::Unsupported(other.to_owned()),
        }
    }

    fn parse_spline(
        &mut self,
        json: &Value,
        files: &HashMap<String, Value>,
        noises: &HashMap<String, Arc<Normal>>,
        depth: usize,
    ) -> (Arc<Node>, Arc<Spline>) {
        let coordinate = match json.get("coordinate") {
            Some(value) => Arc::new(self.parse(value, files, noises, depth)),
            None => Arc::new(Node::Constant(0.0)),
        };
        let mut locations = Vec::new();
        let mut derivatives = Vec::new();
        let mut values = Vec::new();
        for point in json.get("points").and_then(Value::as_array).unwrap_or(&Vec::new()) {
            locations.push(point.get("location").and_then(Value::as_f64).unwrap_or(0.0) as f32);
            derivatives.push(point.get("derivative").and_then(Value::as_f64).unwrap_or(0.0) as f32);
            values.push(match point.get("value") {
                Some(Value::Number(n)) => SplineValue::Fixed(n.as_f64().unwrap_or(0.0) as f32),
                Some(nested @ Value::Object(_)) => {
                    let (coordinate, points) = self.parse_spline(nested, files, noises, depth + 1);
                    SplineValue::Nested { coordinate, points }
                }
                _ => SplineValue::Fixed(0.0),
            });
        }
        (
            coordinate,
            Arc::new(Spline {
                locations,
                derivatives,
                values,
            }),
        )
    }
}

impl Node {
    /// What this function is worth at a block.
    pub fn sample(&self, x: i32, y: i32, z: i32) -> f32 {
        match self {
            Node::Constant(value) => *value,
            Node::Reference(inner) => inner.sample(x, y, z),
            Node::Noise {
                noise,
                xz_scale,
                y_scale,
                shift_x,
                shift_y,
                shift_z,
            } => {
                let sx = x as f64 * xz_scale + shift_x.sample(x, y, z) as f64;
                let sy = y as f64 * y_scale + shift_y.sample(x, y, z) as f64;
                let sz = z as f64 * xz_scale + shift_z.sample(x, y, z) as f64;
                noise.get(sx, sy, sz)
            }
            // The offset noise read flat, and the same noise read with its
            // axes swapped round, which is how the game bends the climate
            // out of line with the grid.
            Node::ShiftA(noise) => noise.get(x as f64 * 0.25, 0.0, z as f64 * 0.25) * 4.0,
            Node::ShiftB(noise) => noise.get(z as f64 * 0.25, x as f64 * 0.25, 0.0) * 4.0,
            Node::Shift(noise) => noise.get(x as f64 * 0.25, y as f64 * 0.25, z as f64 * 0.25) * 4.0,
            Node::Gradient {
                axis,
                from_coordinate,
                to_coordinate,
                from_value,
                to_value,
            } => {
                let along = match axis {
                    Axis::X => x,
                    Axis::Y => y,
                    Axis::Z => z,
                };
                let span = (to_coordinate - from_coordinate) as f32;
                let t = ((along - from_coordinate) as f32 / span).clamp(0.0, 1.0);
                from_value + t * (to_value - from_value)
            }
            Node::Add(a, b) => a.sample(x, y, z) + b.sample(x, y, z),
            Node::Sub(a, b) => a.sample(x, y, z) - b.sample(x, y, z),
            Node::Mul(a, b) => a.sample(x, y, z) * b.sample(x, y, z),
            Node::Div(a, b) => a.sample(x, y, z) / b.sample(x, y, z),
            Node::Min(a, b) => a.sample(x, y, z).min(b.sample(x, y, z)),
            Node::Max(a, b) => a.sample(x, y, z).max(b.sample(x, y, z)),
            Node::Negate(a) => -a.sample(x, y, z),
            Node::Abs(a) => a.sample(x, y, z).abs(),
            Node::Square(a) => {
                let value = a.sample(x, y, z);
                value * value
            }
            Node::Cube(a) => {
                let value = a.sample(x, y, z);
                value * value * value
            }
            Node::HalfNegative(a) => {
                let value = a.sample(x, y, z);
                if value > 0.0 {
                    value
                } else {
                    value * 0.5
                }
            }
            Node::QuarterNegative(a) => {
                let value = a.sample(x, y, z);
                if value > 0.0 {
                    value
                } else {
                    value * 0.25
                }
            }
            Node::Squeeze(a) => {
                let value = a.sample(x, y, z).clamp(-1.0, 1.0);
                value / 2.0 - value * value * value / 24.0
            }
            Node::Clamp { input, min, max } => input.sample(x, y, z).clamp(*min, *max),
            Node::Lerp { alpha, first, second } => {
                let alpha = alpha.sample(x, y, z);
                let first = first.sample(x, y, z);
                first + alpha * (second.sample(x, y, z) - first)
            }
            Node::Spline { coordinate, points } => points.sample(coordinate, x, y, z),
            Node::RangeChoice {
                input,
                min,
                max,
                in_range,
                out_of_range,
            } => {
                let value = input.sample(x, y, z);
                if value >= *min && value < *max {
                    in_range.sample(x, y, z)
                } else {
                    out_of_range.sample(x, y, z)
                }
            }
            Node::OldBlended(noise) => noise.get(x as f64, y as f64, z as f64),
            Node::Interpolated { input, cell_xz, cell_y } => {
                let (in_x, in_y, in_z) = (x.rem_euclid(*cell_xz), y.rem_euclid(*cell_y), z.rem_euclid(*cell_xz));
                if in_x == 0 && in_y == 0 && in_z == 0 {
                    return input.sample(x, y, z);
                }
                // The eight corners of the cell this block sits in.
                let (x0, y0, z0) = (x - in_x, y - in_y, z - in_z);
                let corner = |dx: i32, dy: i32, dz: i32| input.sample(x0 + dx * cell_xz, y0 + dy * cell_y, z0 + dz * cell_xz);
                lerp3(
                    in_x as f32 / *cell_xz as f32,
                    in_y as f32 / *cell_y as f32,
                    in_z as f32 / *cell_xz as f32,
                    corner(0, 0, 0),
                    corner(1, 0, 0),
                    corner(0, 1, 0),
                    corner(1, 1, 0),
                    corner(0, 0, 1),
                    corner(1, 0, 1),
                    corner(0, 1, 1),
                    corner(1, 1, 1),
                )
            }
            Node::IntervalSelect {
                input,
                thresholds,
                choices,
            } => {
                let value = input.sample(x, y, z);
                let index = thresholds
                    .iter()
                    .position(|threshold| value < *threshold)
                    .unwrap_or(choices.len().saturating_sub(1));
                match choices.get(index) {
                    Some(choice) => choice.sample(x, y, z),
                    None => 0.0,
                }
            }
            // There is no older world to blend with here.
            Node::BlendAlpha => 1.0,
            Node::BlendOffset => 0.0,
            Node::Unsupported(_) => 0.0,
        }
    }
}

impl Spline {
    fn value_at(&self, index: usize, x: i32, y: i32, z: i32) -> f32 {
        match &self.values[index] {
            SplineValue::Fixed(value) => *value,
            SplineValue::Nested { coordinate, points } => points.sample(coordinate, x, y, z),
        }
    }

    fn sample(&self, coordinate: &Arc<Node>, x: i32, y: i32, z: i32) -> f32 {
        if self.locations.is_empty() {
            return 0.0;
        }
        let input = coordinate.sample(x, y, z);
        // The interval this input falls in, or off either end.
        let start = self.locations.partition_point(|location| *location <= input) as i32 - 1;
        let last = self.locations.len() - 1;
        if start < 0 {
            return self.extend(input, 0, self.value_at(0, x, y, z));
        }
        let start = start as usize;
        if start == last {
            return self.extend(input, last, self.value_at(last, x, y, z));
        }
        let x1 = self.locations[start];
        let x2 = self.locations[start + 1];
        let t = (input - x1) / (x2 - x1);
        let y1 = self.value_at(start, x, y, z);
        let y2 = self.value_at(start + 1, x, y, z);
        let d1 = self.derivatives[start];
        let d2 = self.derivatives[start + 1];
        let a = d1 * (x2 - x1) - (y2 - y1);
        let b = -d2 * (x2 - x1) + (y2 - y1);
        let lerp = |t: f32, from: f32, to: f32| from + t * (to - from);
        lerp(t, y1, y2) + t * (1.0 - t) * lerp(t, a, b)
    }

    /// Past either end the curve carries on in a straight line.
    fn extend(&self, input: f32, index: usize, value: f32) -> f32 {
        let derivative = self.derivatives[index];
        if derivative == 0.0 {
            value
        } else {
            value + derivative * (input - self.locations[index])
        }
    }
}

/// Reads every noise the data pack defines and seeds it for this world.
fn load_noises(datapack: &Path, seed: i64) -> HashMap<String, Arc<Normal>> {
    let root = datapack.join("minecraft").join("worldgen").join("noise");
    let mut files = HashMap::new();
    collect_files(&root, &root, &mut files);
    let forker = Xoroshiro::from_seed(seed).fork_positional();
    let mut out = HashMap::new();
    for (name, json) in files {
        let amplitudes: Vec<f64> = json
            .get("amplitude_modifiers")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        let parameters = Parameters::from_data(
            json.get("base_octave").and_then(Value::as_i64).unwrap_or(0) as i32,
            json.get("base_amplitude").and_then(Value::as_f64).unwrap_or(1.0),
            json.get("octave_count").and_then(Value::as_i64).unwrap_or(1) as usize,
            &amplitudes,
        );
        let mut random = forker.from_hash_of(&name);
        out.insert(name, Arc::new(Normal::new(&parameters, &mut random)));
    }
    out
}

/// Walks a folder of JSON, naming each file the way the data pack names
/// it: `minecraft:<path below the folder>`.
fn collect_files(root: &Path, dir: &Path, out: &mut HashMap<String, Value>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else { continue };
        let name = format!(
            "minecraft:{}",
            relative.with_extension("").to_string_lossy().replace('\\', "/")
        );
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if let Ok(json) = serde_json::from_str::<Value>(&text) {
            out.insert(name, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datapack() -> Option<std::path::PathBuf> {
        let base = std::env::var("GARNET_DATA").ok()?;
        let candidate = std::path::PathBuf::from(base).join("versions/26.3/datapack");
        candidate
            .join("minecraft/worldgen/density_function/overworld/final_density.json")
            .exists()
            .then_some(candidate)
    }

    /// Each piece the terrain is built from, against the game's own
    /// numbers: the old noise the land is shaped by, the slope and the
    /// factor over it, and every one of the caves.
    #[test]
    fn each_piece_of_the_terrain_matches() {
        let Some(datapack) = datapack() else {
            eprintln!("no prepared data pack found; skipping");
            return;
        };
        let graph = Graph::load(&datapack, 1234567890123);
        let points = [(0, 64, 0), (0, 8, 0), (100, 72, -250), (5000, 90, 5000)];
        let expected: &[(&str, [f32; 4])] = &[
            ("overworld/base_3d_noise", [0.018798314, 0.015147924, 0.087911874, -0.2507416]),
            ("overworld/sloped_cheese", [-0.519929, 8.014228, 0.15161897, -1.541898]),
            ("overworld/factor", [5.8022795, 5.8022795, 1.56, 3.95]),
            ("overworld/jaggedness", [0.0, 0.0, 0.0, 0.0]),
            ("overworld/caves/entrances", [0.24991971, 0.19658692, 0.16984817, 0.28046948]),
            ("overworld/caves/noodle", [0.8114858, 64.0, 0.5812746, 64.0]),
            (
                "overworld/caves/pillars",
                [-0.027588854, -0.011000095, -0.30190367, 0.0089318855],
            ),
            ("overworld/caves/spaghetti_2d", [1.0, 1.0, 1.0, 1.0]),
            (
                "overworld/caves/spaghetti_roughness_function",
                [0.01405247, -8.3049096e-4, 0.01739272, 0.009900187],
            ),
        ];
        for (name, wants) in expected {
            let node = graph.function(&format!("minecraft:{name}")).expect(name);
            for ((x, y, z), want) in points.iter().zip(wants.iter()) {
                let got = node.sample(*x, *y, *z);
                assert!((got - want).abs() < 1e-4, "{name} at {x},{y},{z}: {got} vs {want}");
            }
        }
    }

    /// What the game's own functions come to at six places, against what
    /// this makes of the same graph. `final_density` is the one that
    /// decides rock from air, and it stands on all the others.
    #[test]
    fn terrain_density_matches_the_game() {
        let Some(datapack) = datapack() else {
            eprintln!("no prepared data pack found; skipping");
            return;
        };
        let graph = Graph::load(&datapack, 1234567890123);
        let function = |name: &str| graph.function(name).expect(name);
        let expected: &[(i32, i32, i32, f32, f32, f32, f32, f32)] = &[
            (0, 64, 0, -0.16484208, -0.092847526, 0.22936258, 0.4857032, -0.052071746),
            (0, 8, 0, 0.06282483, 0.34465247, 0.22936258, 0.4857032, -0.052071746),
            (100, 72, -250, 0.046571, 0.010209471, 0.6648526, 0.24994996, 0.7398364),
            (-1500, 40, 3000, 0.019839544, 0.48917407, 1.0020655, -0.1732395, -0.40692088),
            (5000, 90, 5000, -0.4466196, -0.32687503, -0.3896563, -0.27823895, 0.064396665),
            (37, 120, -412, -0.45833334, -0.46871346, 0.64894825, 0.051453687, -0.038865805),
        ];
        for (x, y, z, final_density, depth, continents, erosion, ridges) in expected {
            let check = |name: &str, want: f32| {
                let got = function(name).sample(*x, *y, *z);
                assert!((got - want).abs() < 1e-5, "{name} at {x},{y},{z}: {got} vs {want}");
            };
            check("minecraft:overworld/depth", *depth);
            check("minecraft:overworld/continents", *continents);
            check("minecraft:overworld/erosion", *erosion);
            check("minecraft:overworld/ridges", *ridges);
            check("minecraft:overworld/final_density", *final_density);
        }
    }
}

fn with_namespace(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}
