# Matching vanilla world generation

The goal: the same seed makes the same world in Garnet as in Minecraft.
Not "similar terrain" -- the same blocks in the same places, so a seed
shared on a forum finds the same village.

## How it is checked

The game itself is the reference. Every version Garnet runs has already
downloaded Mojang's server jar and a Java runtime to generate the data
reports, and that runtime is a full JDK. So a short Java program compiled
against the jar can print what the game's own classes produce, and the
Rust tests are written against those numbers.

    scratchpad/oracle/Oracle.java     the program
    scratchpad/oracle/cp.txt          its classpath: the inner server jar
                                      plus the bundled libraries

Run it with the downloaded runtime:

    <data>/java/java-runtime-epsilon/bin/javac.exe -cp <inner jar> -d classes Oracle.java
    <data>/java/java-runtime-epsilon/bin/java.exe -cp @cp.txt Oracle

Later on the check gets coarser and more convincing: run the vanilla
server headless on a seed, let it write region files, run Garnet on the
same seed, and compare the chunks block for block.

## The order of work

1. **Random sources** -- done. `vanilla::rng`: xoroshiro128++ with the
   game's seed scrambling, `java.util.Random` for the older paths, and the
   forks that hand a named or placed generator its own stream. Note 26.3
   works its doubles and floats out in single precision, constant and all;
   getting that wrong moves every number downstream.
2. **Noise** -- the gradient noise, the octave stack over it, and the
   normal noise that pairs two of them. 26.3 rewrote these in floats with
   its own buffer types, so the shape differs from what is written up for
   1.21 elsewhere.
3. **Climate and biomes** -- six noises (temperature, vegetation,
   continentalness, erosion, depth, weirdness) and the parameter list in
   the data pack that says which biome each corner of that space is.
4. **Terrain** -- the density function graph, which the data pack holds in
   full: an interpreter for it, then the noise settings for the overworld.
5. **Surface** -- the surface rule tree, also data, which decides grass
   over dirt over stone, sand in deserts, and so on.
6. **Carvers and features** -- caves and ravines, then ores, trees, lakes
   and the rest, each seeded per chunk in a fixed order.
7. **Structures** -- placement by the structure sets, then the jigsaw
   assembly that villages and the like are built from.

Each step is worth having on its own: biomes alone give the world its
variety, and terrain alone makes seeds recognisable.
