//! Streams chunks to a player as they move: sends the ones that came into
//! view, forgets the ones that left it, and paces sends the way the client
//! asks (`chunk_batch_received` tells us how many chunks per tick it wants).

use crate::player::Player;
use crate::server::Server;
use garnet_protocol::packets::play::clientbound as cb;
use garnet_protocol::ChunkPos;
use std::collections::HashSet;
use std::sync::Arc;

/// Vanilla allows this many batches in flight before waiting for an ack.
const MAX_UNACKED_BATCHES: u32 = 10;

pub fn stream_chunks(server: &Arc<Server>, player: &Arc<Player>, server_view: i32) {
    let (center, view, loaded, unacked, per_tick, last_center) = {
        let state = player.lock();
        (
            state.chunk(),
            state.view_distance.clamp(2, server_view),
            state.loaded_chunks.clone(),
            state.unacked_batches,
            state.chunks_per_tick,
            state.center_chunk,
        )
    };

    if center != last_center {
        player.lock().center_chunk = center;
        player.send(&cb::SetCenterChunk {
            chunk_x: center.x,
            chunk_z: center.z,
        });
    }

    // Everything within the view square is wanted; one extra ring is kept
    // loaded on the server side but not sent, so walking back is instant.
    let wanted: HashSet<ChunkPos> = (-view..=view)
        .flat_map(|dx| (-view..=view).map(move |dz| ChunkPos::new(center.x + dx, center.z + dz)))
        .collect();

    // Forget chunks that left the view.
    let gone: Vec<ChunkPos> = loaded.iter().filter(|p| !wanted.contains(p)).copied().collect();
    if !gone.is_empty() {
        let mut watchers = server.chunk_watchers.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = player.lock();
        for pos in &gone {
            state.loaded_chunks.remove(pos);
            if let Some(set) = watchers.get_mut(pos) {
                set.remove(&player.uuid);
                if set.is_empty() {
                    watchers.remove(pos);
                }
            }
        }
        drop(state);
        drop(watchers);
        for pos in gone {
            player.send(&cb::UnloadChunk {
                chunk_x: pos.x,
                chunk_z: pos.z,
            });
        }
    }

    if unacked >= MAX_UNACKED_BATCHES {
        return;
    }

    // Send missing chunks nearest-first, a few per tick.
    let mut missing: Vec<ChunkPos> = wanted.iter().filter(|p| !loaded.contains(p)).copied().collect();
    if missing.is_empty() {
        return;
    }
    missing.sort_by_key(|p| p.distance_to(center));
    let budget = per_tick.clamp(1.0, 32.0).ceil() as usize;

    let mut batch: Vec<(ChunkPos, bytes::Bytes)> = Vec::with_capacity(budget);
    for pos in missing {
        if batch.len() >= budget {
            break;
        }
        match server.chunk_packet(pos) {
            Some(bytes) => batch.push((pos, bytes)),
            None => server.request_chunk(pos),
        }
    }
    if batch.is_empty() {
        return;
    }

    player.send(&cb::ChunkBatchStart);
    {
        let mut watchers = server.chunk_watchers.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = player.lock();
        for (pos, _) in &batch {
            state.loaded_chunks.insert(*pos);
            watchers.entry(*pos).or_default().insert(player.uuid);
        }
        state.unacked_batches += 1;
    }
    let count = batch.len() as i32;
    for (_, bytes) in batch {
        player.send_raw(bytes);
    }
    player.send(&cb::ChunkBatchFinished { batch_size: count });
}
