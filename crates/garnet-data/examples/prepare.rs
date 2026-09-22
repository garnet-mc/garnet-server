//! Downloads and prepares game data for a version: `cargo run --example prepare -- <data dir> [version]`.
use garnet_data::{GameData, VersionChoice};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let mut args = std::env::args().skip(1);
    let data_dir = std::path::PathBuf::from(args.next().unwrap_or_else(|| "data".into()));
    let choice = VersionChoice::parse(&args.next().unwrap_or_default());
    let data = GameData::load(&data_dir, &choice).await?;
    println!("version {} protocol {} data version {}", data.version, data.protocol_version, data.data_version);
    println!("stone default state = {:?}", data.blocks.default_state("stone"));
    println!("grass_block default state = {:?}", data.blocks.default_state("grass_block"));
    println!("bits per block state = {}", data.blocks.bits_per_state());
    println!("player entity type id = {:?}", data.registries.id_of("entity_type", "player"));
    println!("overworld dimension type id = {:?}", data.dynamic.id_of("dimension_type", "overworld"));
    println!("plains biome id = {:?}", data.dynamic.id_of("worldgen/biome", "plains"));
    println!("login packet id = {:?}", data.packet_ids.clientbound_id(garnet_protocol::State::Play, "login"));
    for reg in &data.dynamic.registries {
        println!("  registry {} has {} entries", reg.id, reg.entries.len());
    }
    Ok(())
}
