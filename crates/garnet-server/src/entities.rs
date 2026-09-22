//! Other players as entities: who can see whom, and movement updates.
//!
//! A client is told about another player's entity once it has that player's
//! chunk loaded, and told to remove it when the chunk goes away. Movement
//! packets only go to players watching the mover's chunk.

use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::clientbound as cb;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

/// Spawns and despawns other players' entities for `player` based on which
/// chunks it has loaded. Runs a few times per second per player.
pub fn update_visibility(server: &Arc<Server>, player: &Arc<Player>) {
    let (loaded, visible, ready) = {
        let state = player.lock();
        (state.loaded_chunks.clone(), state.visible_players.clone(), state.loaded)
    };
    if !ready {
        return;
    }
    let should_see: HashSet<Uuid> = {
        let by_chunk = server.players_by_chunk.lock().unwrap_or_else(|e| e.into_inner());
        by_chunk
            .iter()
            .filter(|(chunk, _)| loaded.contains(chunk))
            .flat_map(|(_, set)| set.iter().copied())
            .filter(|uuid| *uuid != player.uuid)
            .collect()
    };

    let appeared: Vec<Uuid> = should_see.difference(&visible).copied().collect();
    let vanished: Vec<Uuid> = visible.difference(&should_see).copied().collect();
    if appeared.is_empty() && vanished.is_empty() {
        return;
    }

    for uuid in &appeared {
        if let Some(other) = server.player(*uuid) {
            player.send(&spawn_packet(server, &other));
            let (yaw, sneaking) = {
                let s = other.lock();
                (s.yaw, s.sneaking)
            };
            player.send(&cb::RotateHead {
                entity_id: other.entity_id,
                head_yaw: yaw,
            });
            if sneaking {
                player.send(&pose_packet(&other, true));
            }
        }
    }
    if !vanished.is_empty() {
        let ids: Vec<i32> = vanished
            .iter()
            .filter_map(|u| server.player(*u).map(|p| p.entity_id))
            .collect();
        if !ids.is_empty() {
            player.send(&cb::RemoveEntities { entity_ids: ids });
        }
    }

    let mut state = player.lock();
    for uuid in appeared {
        state.visible_players.insert(uuid);
    }
    for uuid in vanished {
        state.visible_players.remove(&uuid);
    }
}

pub fn spawn_packet(server: &Server, other: &Player) -> cb::SpawnEntity {
    let state = other.lock();
    cb::SpawnEntity {
        entity_id: other.entity_id,
        uuid: other.uuid,
        entity_type: server.data.registries.id_of("entity_type", "player").unwrap_or(0),
        x: state.x,
        y: state.y,
        z: state.z,
        velocity: (0.0, 0.0, 0.0),
        pitch: state.pitch,
        yaw: state.yaw,
        head_yaw: state.yaw,
        data: 0,
    }
}

/// Entity metadata for crouching: index 0 is the flags byte (bit 1 =
/// sneaking) and index 6 the pose (type id 20; pose 5 = crouching).
pub fn pose_packet(player: &Player, sneaking: bool) -> cb::SetEntityData {
    let mut flags = garnet_protocol::PacketWriter::new();
    flags.write_u8(if sneaking { 0x02 } else { 0x00 });
    let mut pose = garnet_protocol::PacketWriter::new();
    pose.write_varint(if sneaking { 5 } else { 0 });
    cb::SetEntityData {
        entity_id: player.entity_id,
        entries: vec![(0, 0, flags.into_inner()), (6, 20, pose.into_inner())],
    }
}

/// Tells nearby players that `player` moved from `from` to its current
/// position. Small moves use the delta packet, big ones a full sync.
pub fn broadcast_movement(server: &Server, player: &Player, from: (f64, f64, f64), rotated: bool) {
    let (to, yaw, pitch, on_ground, chunk) = {
        let s = player.lock();
        ((s.x, s.y, s.z), s.yaw, s.pitch, s.on_ground, s.chunk())
    };
    let moved = to != from;
    let big_move = (to.0 - from.0).abs() >= 7.9 || (to.1 - from.1).abs() >= 7.9 || (to.2 - from.2).abs() >= 7.9;
    if big_move {
        server.broadcast_near(
            chunk,
            &cb::EntityPositionSync {
                entity_id: player.entity_id,
                x: to.0,
                y: to.1,
                z: to.2,
                yaw,
                pitch,
                on_ground,
            },
            Some(player.uuid),
        );
    } else if moved && rotated {
        server.broadcast_near(
            chunk,
            &cb::MoveEntityPosRot {
                entity_id: player.entity_id,
                delta_x: cb::movement_delta(from.0, to.0),
                delta_y: cb::movement_delta(from.1, to.1),
                delta_z: cb::movement_delta(from.2, to.2),
                yaw,
                pitch,
                on_ground,
            },
            Some(player.uuid),
        );
    } else if moved {
        server.broadcast_near(
            chunk,
            &cb::MoveEntityPos {
                entity_id: player.entity_id,
                delta_x: cb::movement_delta(from.0, to.0),
                delta_y: cb::movement_delta(from.1, to.1),
                delta_z: cb::movement_delta(from.2, to.2),
                on_ground,
            },
            Some(player.uuid),
        );
    } else if rotated {
        server.broadcast_near(
            chunk,
            &cb::MoveEntityRot {
                entity_id: player.entity_id,
                yaw,
                pitch,
                on_ground,
            },
            Some(player.uuid),
        );
    }
    if rotated {
        server.broadcast_near(
            chunk,
            &cb::RotateHead {
                entity_id: player.entity_id,
                head_yaw: yaw,
            },
            Some(player.uuid),
        );
    }
}
