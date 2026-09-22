//! What blocks do when you touch them or when the world moves under them:
//! doors that open, buttons that pop back out, sand that falls, crops that
//! grow, and beds that see the night out.

use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::packets::play::GameMode;
use garnet_protocol::{BlockPos, Text};
use std::collections::BTreeMap;
use std::sync::Arc;

/// How long a button stays pressed.
const STONE_BUTTON_TICKS: u64 = 20;
const WOODEN_BUTTON_TICKS: u64 = 30;
/// A block falls this far per tick once it starts.
const FALL_STEP: i32 = 1;
/// Night, in the day's 24000 ticks: when you may go to bed.
const NIGHT_START: i64 = 12542;
const NIGHT_END: i64 = 23460;

/// Runs the block ticks that have come due, then gives a few random
/// blocks near each player their chance to grow.
pub fn tick(server: &Arc<Server>, tick: u64) {
    let due: Vec<BlockPos> = {
        let mut ticks = server.block_ticks.lock().unwrap_or_else(|e| e.into_inner());
        let (ready, waiting): (Vec<_>, Vec<_>) = ticks.drain(..).partition(|(_, when)| *when <= tick);
        *ticks = waiting;
        ready.into_iter().map(|(pos, _)| pos).collect()
    };
    for pos in due {
        scheduled(server, pos);
    }
    let speed = server
        .rules
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .game_rule_int("randomTickSpeed")
        .unwrap_or(3);
    if speed <= 0 {
        return;
    }
    // Vanilla picks a few blocks in every section of every loaded chunk.
    // We keep to the chunks around each player and the sections they can
    // reach, which is where anyone would notice a crop standing still.
    let mut seen: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
    for player in server.online_players() {
        let (px, py, pz) = {
            let s = player.lock();
            (s.x.floor() as i32, s.y.floor() as i32, s.z.floor() as i32)
        };
        for cx in (px >> 4) - 1..=(px >> 4) + 1 {
            for cz in (pz >> 4) - 1..=(pz >> 4) + 1 {
                if !seen.insert((cx, cz)) {
                    continue;
                }
                for section in -2..=2 {
                    let base_y = ((py >> 4) + section) << 4;
                    for _ in 0..speed {
                        let pos = BlockPos::new(
                            (cx << 4) + rand::random_range(0..16),
                            (base_y + rand::random_range(0..16)).clamp(-63, 319),
                            (cz << 4) + rand::random_range(0..16),
                        );
                        random_tick(server, pos);
                    }
                }
            }
        }
    }
}

/// Right-clicking a block that does something. True when it was handled
/// and nothing should be placed.
pub fn interact(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos, block: &str, props: &BTreeMap<String, String>) -> bool {
    let short = block.strip_prefix("minecraft:").unwrap_or(block);
    if short.ends_with("_door") || short.ends_with("_trapdoor") || short.ends_with("_fence_gate") {
        // Iron stays shut without redstone, as it does in vanilla.
        if short.starts_with("iron_") {
            return false;
        }
        return toggle(server, pos, block, props, "open");
    }
    if short.ends_with("_button") {
        let ticks = if short.contains("stone") { STONE_BUTTON_TICKS } else { WOODEN_BUTTON_TICKS };
        if toggle_to(server, pos, block, props, "powered", true) {
            server.schedule_block(pos, ticks);
            return true;
        }
        return false;
    }
    if short.ends_with("_lever") || short == "lever" {
        return toggle(server, pos, block, props, "powered");
    }
    if short.ends_with("_bed") {
        return sleep(server, player, pos);
    }
    false
}

/// Flips a true/false property.
fn toggle(server: &Arc<Server>, pos: BlockPos, block: &str, props: &BTreeMap<String, String>, name: &str) -> bool {
    let now = props.get(name).map(String::as_str) == Some("true");
    toggle_to(server, pos, block, props, name, !now)
}

fn toggle_to(server: &Arc<Server>, pos: BlockPos, block: &str, props: &BTreeMap<String, String>, name: &str, value: bool) -> bool {
    let mut next = props.clone();
    if !next.contains_key(name) {
        return false;
    }
    next.insert(name.to_owned(), value.to_string());
    let Some(state) = server.data.blocks.state_with(block, &next) else { return false };
    server.set_block(pos, state as u32);
    // A double door's other half moves with it.
    if block.ends_with("_door") {
        let other = match next.get("half").map(String::as_str) {
            Some("lower") => pos.offset(0, 1, 0),
            Some("upper") => pos.offset(0, -1, 0),
            _ => return true,
        };
        // Read first, then act: set_block takes the world lock itself, so
        // nothing here may still be holding it.
        let other_state = server.world().get_block(other).ok();
        let blocks = &server.data.blocks;
        let matching = other_state
            .filter(|state| blocks.block_of_state(*state as i32).map(|b| b.name.as_str()) == Some(block))
            .and_then(|state| blocks.state(state as i32).map(|s| s.properties.clone()));
        if let Some(mut other_props) = matching {
            other_props.insert(name.to_owned(), value.to_string());
            if let Some(id) = blocks.state_with(block, &other_props) {
                server.set_block(other, id as u32);
            }
        }
    }
    true
}

/// Going to bed: sets your spawn, and if everyone turns in, ends the night.
fn sleep(server: &Arc<Server>, player: &Arc<Player>, pos: BlockPos) -> bool {
    let (time, raining) = {
        let world = server.world();
        (world.settings.time_of_day % 24000, false)
    };
    player.lock().spawn_point = Some(pos);
    player.send(&cb::SystemChat {
        content: Text::new("Respawn point set"),
        overlay: true,
    });
    if crate::mobs::monsters_near(server, pos) {
        player.send(&cb::SystemChat {
            content: Text::new("You may not rest now, there are monsters nearby"),
            overlay: true,
        });
        return true;
    }
    let night = (NIGHT_START..NIGHT_END).contains(&time) || raining;
    if !night {
        player.send(&cb::SystemChat {
            content: Text::new("You can only sleep at night"),
            overlay: true,
        });
        return true;
    }
    player.lock().sleeping = true;
    let (asleep, awake): (usize, usize) = server
        .online_players()
        .iter()
        .filter(|p| matches!(p.lock().game_mode, GameMode::Survival | GameMode::Adventure))
        .fold((0, 0), |(asleep, awake), p| {
            if p.lock().sleeping {
                (asleep + 1, awake)
            } else {
                (asleep, awake + 1)
            }
        });
    let needed = server
        .rules
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .game_rule_int("playersSleepingPercentage")
        .unwrap_or(100);
    let total = asleep + awake;
    let enough = total > 0 && (asleep * 100) >= (total * needed.max(0) as usize);
    if enough {
        morning(server);
    } else {
        server.broadcast_chat(Text::new(format!(
            "{} is sleeping ({asleep}/{total})",
            player.name()
        )));
    }
    true
}

/// Morning: everyone wakes up and the clock jumps.
fn morning(server: &Arc<Server>) {
    {
        let mut world = server.world();
        let day = world.settings.time_of_day / 24000;
        world.settings.time_of_day = (day + 1) * 24000;
    }
    for player in server.online_players() {
        player.lock().sleeping = false;
    }
    server.broadcast_chat(Text::new("Good morning"));
}

/// Anything the player woke up from: leaving a bed, mostly.
pub fn wake(player: &Arc<Player>) {
    player.lock().sleeping = false;
}

/// A block changed: see whether its neighbours care.
pub fn changed(server: &Arc<Server>, pos: BlockPos) {
    // Only what sits directly above can fall into the gap.
    let above = pos.offset(0, 1, 0);
    if falls(server, above) {
        server.schedule_block(above, 2);
    }
    crate::fluids::wake_neighbours(server, pos);
}

/// Whether the block here is one that falls when nothing holds it up.
fn falls(server: &Arc<Server>, pos: BlockPos) -> bool {
    let Ok(state) = server.world().get_block(pos) else { return false };
    let Some(block) = server.data.blocks.block_of_state(state as i32) else { return false };
    let short = block.name.strip_prefix("minecraft:").unwrap_or(&block.name);
    short.ends_with("sand") || short.ends_with("gravel") || short.contains("concrete_powder") || short.ends_with("anvil")
}

/// A scheduled tick came due: buttons pop out, sand falls, crops grow.
pub fn scheduled(server: &Arc<Server>, pos: BlockPos) {
    let (state, name, props) = {
        let Ok(state) = server.world().get_block(pos) else { return };
        let blocks = &server.data.blocks;
        let Some(block) = blocks.block_of_state(state as i32) else { return };
        let props = blocks.state(state as i32).map(|s| s.properties.clone()).unwrap_or_default();
        (state, block.name.clone(), props)
    };
    let short = name.strip_prefix("minecraft:").unwrap_or(&name);
    if short == "water" || short == "lava" {
        crate::fluids::tick(server, pos);
        return;
    }
    if short.ends_with("_button") && props.get("powered").map(String::as_str) == Some("true") {
        toggle_to(server, pos, &name, &props, "powered", false);
        return;
    }
    if falls(server, pos) {
        fall(server, pos, state);
    }
}

/// Moves a falling block down until something stops it.
fn fall(server: &Arc<Server>, pos: BlockPos, state: u32) {
    let air = server.data.blocks.default_state("air").unwrap_or(0) as u32;
    let mut at = pos;
    loop {
        let below = at.offset(0, -FALL_STEP, 0);
        let Ok(under) = server.world().get_block(below) else { break };
        let blocks = &server.data.blocks;
        if !blocks.is_air(under as i32) && !blocks.is_liquid(under as i32) {
            break;
        }
        at = below;
        let floor = server.world().range.min_y;
        if at.y <= floor {
            break;
        }
    }
    if at != pos {
        server.set_block(pos, air);
        server.set_block(at, state);
        // Whatever was above the old spot may now fall as well.
        changed(server, pos);
    }
}

/// Random ticks: crops ripen, saplings grow up, grass spreads.
pub fn random_tick(server: &Arc<Server>, pos: BlockPos) {
    let (name, props) = {
        let Ok(state) = server.world().get_block(pos) else { return };
        let blocks = &server.data.blocks;
        let Some(block) = blocks.block_of_state(state as i32) else { return };
        let props = blocks.state(state as i32).map(|s| s.properties.clone()).unwrap_or_default();
        (block.name.clone(), props)
    };
    let short = name.strip_prefix("minecraft:").unwrap_or(&name).to_owned();
    // Anything with an age that is not yet at its last stage.
    let Some(age) = props.get("age").and_then(|a| a.parse::<i32>().ok()) else { return };
    if !matches!(
        short.as_str(),
        "wheat" | "carrots" | "potatoes" | "beetroots" | "melon_stem" | "pumpkin_stem" | "torchflower_crop" | "sweet_berry_bush" | "nether_wart" | "cocoa"
    ) {
        return;
    }
    let max = match short.as_str() {
        "beetroots" | "nether_wart" | "sweet_berry_bush" | "cocoa" => 3,
        "torchflower_crop" => 2,
        _ => 7,
    };
    if age >= max {
        return;
    }
    // The random tick is already the slow part; vanilla then rolls again
    // against how good the ground is, which comes out around half.
    if rand::random::<f32>() > 0.5 {
        return;
    }
    let mut next = props.clone();
    next.insert("age".to_owned(), (age + 1).to_string());
    if let Some(state) = server.data.blocks.state_with(&name, &next) {
        server.set_block(pos, state as u32);
    }
}
