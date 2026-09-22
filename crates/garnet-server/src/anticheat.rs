//! Server-side sanity checks on what clients send.
//!
//! The client is never trusted for movement or interaction range. Each check
//! produces violation points; points decay over time, and crossing the
//! configured thresholds kicks and then temporarily bans. Checks are
//! deliberately lenient (false positives are worse than a missed hack) and
//! everything is visible in the panel's anti-cheat page.

use crate::config::AnticheatSection;
use garnet_admin::api::ViolationEntry;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

/// Per-player anti-cheat memory, lives inside the player's state.
#[derive(Debug)]
pub struct PlayerChecks {
    pub points: u32,
    last_decay: Instant,
    /// Horizontal distance moved and upward gain within the current second.
    window_start: Instant,
    horizontal: f64,
    upward: f64,
    /// How long the player has been airborne without moving vertically.
    hover_since: Option<Instant>,
    packets_this_second: u32,
    packet_window: Instant,
    chat_this_second: u32,
    chat_window: Instant,
    /// Set after we teleport the player; checks resume once they confirm.
    pub awaiting_teleport: bool,
}

impl Default for PlayerChecks {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            points: 0,
            last_decay: now,
            window_start: now,
            horizontal: 0.0,
            upward: 0.0,
            hover_since: None,
            packets_this_second: 0,
            packet_window: now,
            chat_this_second: 0,
            chat_window: now,
            awaiting_teleport: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Speed,
    Fly,
    Reach,
    PacketFlood,
    ChatFlood,
    InvalidMovement,
}

impl Check {
    pub fn name(self) -> &'static str {
        match self {
            Check::Speed => "speed",
            Check::Fly => "fly",
            Check::Reach => "reach",
            Check::PacketFlood => "packet-flood",
            Check::ChatFlood => "chat-flood",
            Check::InvalidMovement => "invalid-movement",
        }
    }

    fn points(self) -> u32 {
        match self {
            Check::Speed => 2,
            Check::Fly => 3,
            Check::Reach => 2,
            Check::PacketFlood => 6,
            Check::ChatFlood => 1,
            Check::InvalidMovement => 10,
        }
    }
}

#[derive(Debug)]
pub struct Violation {
    pub check: Check,
    pub details: String,
}

/// What to do after points were added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escalation {
    None,
    Kick,
    Ban,
}

/// What the world looks like around the player, for the fly check.
#[derive(Clone, Copy, Debug, Default)]
pub struct Surroundings {
    pub in_liquid: bool,
    pub on_climbable: bool,
}

pub struct AntiCheat {
    pub config: AnticheatSection,
    recent: Mutex<VecDeque<ViolationEntry>>,
}

impl AntiCheat {
    pub fn new(config: AnticheatSection) -> Self {
        Self {
            config,
            recent: Mutex::new(VecDeque::new()),
        }
    }

    pub fn recent(&self) -> Vec<ViolationEntry> {
        self.recent.lock().unwrap_or_else(|e| e.into_inner()).iter().rev().cloned().collect()
    }

    fn decay(&self, checks: &mut PlayerChecks) {
        let secs = checks.last_decay.elapsed().as_secs();
        let step = self.config.decay_seconds.max(1);
        if secs >= step {
            let lost = (secs / step) as u32;
            checks.points = checks.points.saturating_sub(lost);
            checks.last_decay = Instant::now();
        }
    }

    /// Records a violation and returns whether to kick or ban.
    pub fn flag(&self, player: &str, checks: &mut PlayerChecks, violation: &Violation) -> Escalation {
        self.decay(checks);
        checks.points += violation.check.points();
        tracing::warn!(
            "anticheat: {player} failed {} ({}), {} points",
            violation.check.name(),
            violation.details,
            checks.points
        );
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        if recent.len() >= 500 {
            recent.pop_front();
        }
        recent.push_back(ViolationEntry {
            time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            player: player.to_owned(),
            check: violation.check.name().to_owned(),
            details: violation.details.clone(),
            level: checks.points,
        });
        if self.config.ban_threshold > 0 && checks.points >= self.config.ban_threshold {
            Escalation::Ban
        } else if checks.points >= self.config.kick_threshold {
            Escalation::Kick
        } else {
            Escalation::None
        }
    }

    /// Validates a movement packet. `from` is the last accepted position.
    /// `flying` covers creative flight, spectator and elytra.
    pub fn check_movement(
        &self,
        checks: &mut PlayerChecks,
        from: (f64, f64, f64),
        to: (f64, f64, f64),
        on_ground: bool,
        flying: bool,
        around: Surroundings,
    ) -> Option<Violation> {
        if !self.config.enabled || checks.awaiting_teleport {
            return None;
        }
        if !(to.0.is_finite() && to.1.is_finite() && to.2.is_finite()) || to.0.abs() > 3.0e7 || to.2.abs() > 3.0e7 {
            return Some(Violation {
                check: Check::InvalidMovement,
                details: format!("position {:?}", to),
            });
        }

        // Accumulate distances over one-second windows; comparing per packet
        // would false-flag every lag spike.
        if checks.window_start.elapsed().as_secs_f64() >= 1.0 {
            checks.window_start = Instant::now();
            checks.horizontal = 0.0;
            checks.upward = 0.0;
        }
        let dx = to.0 - from.0;
        let dz = to.2 - from.2;
        let dy = to.1 - from.1;
        checks.horizontal += (dx * dx + dz * dz).sqrt();
        if dy > 0.0 {
            checks.upward += dy;
        }

        // Vanilla: walking 4.3 m/s, sprinting 5.6, sprint-jumping about 7.1,
        // creative flying 10.9 and sprint-flying 21.8, elytra well past 30.
        let max_horizontal = if flying { 40.0 } else { 9.0 } * self.config.speed_tolerance;
        if checks.horizontal > max_horizontal {
            let details = format!("{:.1} blocks in one second (limit {:.1})", checks.horizontal, max_horizontal);
            checks.horizontal = 0.0;
            return Some(Violation {
                check: Check::Speed,
                details,
            });
        }

        if !flying && !around.in_liquid && !around.on_climbable {
            // A jump gains 1.25 blocks; anything sustained beyond that is flight.
            let max_up = 3.0 * self.config.speed_tolerance;
            if checks.upward > max_up {
                let details = format!("rose {:.1} blocks in one second without flying", checks.upward);
                checks.upward = 0.0;
                return Some(Violation {
                    check: Check::Fly,
                    details,
                });
            }
            // Hovering: airborne, no vertical change, for a while.
            if !on_ground && dy.abs() < 1e-4 {
                let since = *checks.hover_since.get_or_insert_with(Instant::now);
                if since.elapsed().as_secs_f64() > 2.5 {
                    checks.hover_since = None;
                    return Some(Violation {
                        check: Check::Fly,
                        details: "hovering in mid-air".into(),
                    });
                }
            } else {
                checks.hover_since = None;
            }
        } else {
            checks.hover_since = None;
        }
        None
    }

    /// Distance from the player's eyes to a block they touched.
    pub fn check_reach(&self, eyes: (f64, f64, f64), block: (i32, i32, i32), creative: bool) -> Option<Violation> {
        if !self.config.enabled {
            return None;
        }
        // Nearest point of the block to the eyes.
        let nearest = |e: f64, b: i32| e.clamp(b as f64, b as f64 + 1.0);
        let (nx, ny, nz) = (nearest(eyes.0, block.0), nearest(eyes.1, block.1), nearest(eyes.2, block.2));
        let dist = ((eyes.0 - nx).powi(2) + (eyes.1 - ny).powi(2) + (eyes.2 - nz).powi(2)).sqrt();
        let limit = if creative { 6.0 } else { 4.5 } + 1.0; // a block of slack for latency
        if dist > limit {
            return Some(Violation {
                check: Check::Reach,
                details: format!("touched a block {dist:.1} blocks away (limit {limit:.1})"),
            });
        }
        None
    }

    /// Counts a received packet; too many per second is a flood.
    pub fn count_packet(&self, checks: &mut PlayerChecks) -> Option<Violation> {
        if checks.packet_window.elapsed().as_secs_f64() >= 1.0 {
            checks.packet_window = Instant::now();
            checks.packets_this_second = 0;
        }
        checks.packets_this_second += 1;
        if self.config.enabled && checks.packets_this_second > self.config.max_packets_per_second {
            checks.packets_this_second = 0;
            return Some(Violation {
                check: Check::PacketFlood,
                details: format!("more than {} packets in one second", self.config.max_packets_per_second),
            });
        }
        None
    }

    pub fn count_chat(&self, checks: &mut PlayerChecks) -> Option<Violation> {
        if checks.chat_window.elapsed().as_secs_f64() >= 1.0 {
            checks.chat_window = Instant::now();
            checks.chat_this_second = 0;
        }
        checks.chat_this_second += 1;
        if self.config.enabled && checks.chat_this_second > self.config.max_chat_per_second {
            return Some(Violation {
                check: Check::ChatFlood,
                details: "sending chat too fast".into(),
            });
        }
        None
    }
}
