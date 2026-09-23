//! Vanilla-compatible world generation: the same seed, the same world.
//!
//! Built from the bottom up and checked at every step against the game
//! itself, which is on hand as a jar and a Java runtime. `scratchpad/oracle`
//! holds the little program that prints the reference numbers the tests
//! here are written against.

pub mod climate;
pub mod density;
pub mod noise;
pub mod rng;
pub mod surface;
