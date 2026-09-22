//! Chunks, blocks, terrain generation and world storage.
//!
//! - `chunk`     – in-memory chunk columns with palette-compressed sections
//! - `encode`    – turning a chunk into the network packet the client wants
//! - `generator` – terrain generators (flat and simple noise)
//! - `anvil`     – reading and writing vanilla `.mca` region files
//! - `compat`    – rename tables so old saves keep loading
//! - `world`     – the `World`: chunk cache, block access, saving

pub mod anvil;
pub mod chunk;
pub mod compat;
pub mod encode;
pub mod generator;
pub mod light;
pub mod world;

pub use chunk::{Chunk, ChunkSection};
pub use generator::{FlatGenerator, NoiseGenerator, WorldGenerator};
pub use world::{World, WorldSettings};

/// Vertical extent of a dimension in blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeightRange {
    pub min_y: i32,
    pub height: i32,
}

impl HeightRange {
    pub const OVERWORLD: HeightRange = HeightRange { min_y: -64, height: 384 };

    pub fn max_y(&self) -> i32 {
        self.min_y + self.height - 1
    }

    pub fn section_count(&self) -> usize {
        (self.height / 16) as usize
    }

    pub fn min_section(&self) -> i32 {
        self.min_y >> 4
    }

    pub fn contains(&self, y: i32) -> bool {
        y >= self.min_y && y <= self.max_y()
    }
}
