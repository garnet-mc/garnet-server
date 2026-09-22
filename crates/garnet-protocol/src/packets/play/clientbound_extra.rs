//! More play packets: sound, particles, world border, boss bars, the
//! scoreboard and teams, mob effects, attributes, and the tick rate. These
//! back the vanilla command set.

use crate::buffer::PacketWriter;
use crate::packets::{ClientboundPacket, State};
use crate::text::Text;
use crate::types::Identifier;
use uuid::Uuid;

/// Where a sound plays from, in the client's volume categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SoundSource {
    Master = 0,
    Music = 1,
    Records = 2,
    Weather = 3,
    Blocks = 4,
    Hostile = 5,
    Neutral = 6,
    Players = 7,
    Ambient = 8,
    Voice = 9,
    Ui = 10,
}

impl SoundSource {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "master" => Self::Master,
            "music" => Self::Music,
            "record" | "records" => Self::Records,
            "weather" => Self::Weather,
            "block" | "blocks" => Self::Blocks,
            "hostile" => Self::Hostile,
            "neutral" => Self::Neutral,
            "player" | "players" => Self::Players,
            "ambient" => Self::Ambient,
            "voice" => Self::Voice,
            "ui" => Self::Ui,
            _ => return None,
        })
    }
}

/// A sound at a position. The sound is given by name so it does not have
/// to be in the registry (resource packs add their own).
pub struct Sound {
    pub name: Identifier,
    /// Registry id of the sound event, if known; sends the compact form.
    pub registry_id: Option<i32>,
    pub source: SoundSource,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub volume: f32,
    pub pitch: f32,
    pub seed: i64,
}

impl ClientboundPacket for Sound {
    const NAME: &'static str = "sound";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        // Holder: registry id + 1, or 0 followed by an inline sound event.
        match self.registry_id {
            Some(id) => w.write_varint(id + 1),
            None => {
                w.write_varint(0);
                w.write_identifier(&self.name);
                w.write_bool(false); // no fixed range
            }
        }
        w.write_varint(self.source as i32);
        w.write_i32((self.x * 8.0) as i32);
        w.write_i32((self.y * 8.0) as i32);
        w.write_i32((self.z * 8.0) as i32);
        w.write_f32(self.volume);
        w.write_f32(self.pitch);
        w.write_i64(self.seed);
    }
}

pub struct StopSound {
    pub source: Option<SoundSource>,
    pub name: Option<Identifier>,
}

impl ClientboundPacket for StopSound {
    const NAME: &'static str = "stop_sound";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        let flags = (self.source.is_some() as u8) | ((self.name.is_some() as u8) << 1);
        w.write_u8(flags);
        if let Some(source) = self.source {
            w.write_varint(source as i32);
        }
        if let Some(name) = &self.name {
            w.write_identifier(name);
        }
    }
}

/// Particles without extra data (most of them). Particles that need data
/// (dust colour, block, item) are not sent by the server yet.
pub struct Particles {
    pub particle_type: i32,
    pub override_limiter: bool,
    pub always_show: bool,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub spread: (f32, f32, f32),
    pub max_speed: f32,
    pub count: i32,
}

impl ClientboundPacket for Particles {
    const NAME: &'static str = "level_particles";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.particle_type);
        w.write_bool(self.override_limiter);
        w.write_bool(self.always_show);
        w.write_f64(self.x);
        w.write_f64(self.y);
        w.write_f64(self.z);
        w.write_f32(self.spread.0);
        w.write_f32(self.spread.1);
        w.write_f32(self.spread.2);
        w.write_f32(self.max_speed);
        w.write_f32(self.max_speed);
        w.write_f32(self.max_speed);
        w.write_varint(self.count);
        w.write_varint(0); // randomization: default
    }
}

pub struct TickingState {
    pub tick_rate: f32,
    pub frozen: bool,
}

impl ClientboundPacket for TickingState {
    const NAME: &'static str = "ticking_state";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_f32(self.tick_rate);
        w.write_bool(self.frozen);
    }
}

pub struct TickingStep {
    pub steps: i32,
}

impl ClientboundPacket for TickingStep {
    const NAME: &'static str = "ticking_step";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.steps);
    }
}

// ---- world border ----

pub struct InitializeBorder {
    pub center_x: f64,
    pub center_z: f64,
    pub old_size: f64,
    pub new_size: f64,
    pub lerp_time_ms: i64,
    pub absolute_max_size: i32,
    pub warning_blocks: i32,
    pub warning_time: i32,
}

impl ClientboundPacket for InitializeBorder {
    const NAME: &'static str = "initialize_border";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_f64(self.center_x);
        w.write_f64(self.center_z);
        w.write_f64(self.old_size);
        w.write_f64(self.new_size);
        w.write_varlong(self.lerp_time_ms);
        w.write_varint(self.absolute_max_size);
        w.write_varint(self.warning_blocks);
        w.write_varint(self.warning_time);
    }
}

pub struct SetBorderCenter {
    pub x: f64,
    pub z: f64,
}

impl ClientboundPacket for SetBorderCenter {
    const NAME: &'static str = "set_border_center";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_f64(self.x);
        w.write_f64(self.z);
    }
}

pub struct SetBorderLerpSize {
    pub old_size: f64,
    pub new_size: f64,
    pub lerp_time_ms: i64,
}

impl ClientboundPacket for SetBorderLerpSize {
    const NAME: &'static str = "set_border_lerp_size";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_f64(self.old_size);
        w.write_f64(self.new_size);
        w.write_varlong(self.lerp_time_ms);
    }
}

pub struct SetBorderSize {
    pub size: f64,
}

impl ClientboundPacket for SetBorderSize {
    const NAME: &'static str = "set_border_size";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_f64(self.size);
    }
}

pub struct SetBorderWarningDelay {
    pub seconds: i32,
}

impl ClientboundPacket for SetBorderWarningDelay {
    const NAME: &'static str = "set_border_warning_delay";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.seconds);
    }
}

pub struct SetBorderWarningDistance {
    pub blocks: i32,
}

impl ClientboundPacket for SetBorderWarningDistance {
    const NAME: &'static str = "set_border_warning_distance";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.blocks);
    }
}

// ---- boss bars ----

pub enum BossEventAction {
    Add {
        title: Text,
        /// 0..1
        progress: f32,
        color: i32,
        division: i32,
        flags: u8,
    },
    Remove,
    Progress(f32),
    Title(Text),
    Style { color: i32, division: i32 },
    Flags(u8),
}

pub struct BossEvent {
    pub uuid: Uuid,
    pub action: BossEventAction,
}

impl ClientboundPacket for BossEvent {
    const NAME: &'static str = "boss_event";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_uuid(&self.uuid);
        match &self.action {
            BossEventAction::Add {
                title,
                progress,
                color,
                division,
                flags,
            } => {
                w.write_varint(0);
                w.write_text(title);
                w.write_f32(*progress);
                w.write_varint(*color);
                w.write_varint(*division);
                w.write_u8(*flags);
            }
            BossEventAction::Remove => w.write_varint(1),
            BossEventAction::Progress(p) => {
                w.write_varint(2);
                w.write_f32(*p);
            }
            BossEventAction::Title(t) => {
                w.write_varint(3);
                w.write_text(t);
            }
            BossEventAction::Style { color, division } => {
                w.write_varint(4);
                w.write_varint(*color);
                w.write_varint(*division);
            }
            BossEventAction::Flags(f) => {
                w.write_varint(5);
                w.write_u8(*f);
            }
        }
    }
}

// ---- scoreboard ----

pub enum ObjectiveMode {
    Create,
    Remove,
    Update,
}

pub struct SetObjective {
    pub name: String,
    pub mode: ObjectiveMode,
    pub display_name: Text,
    /// 0 = integer, 1 = hearts.
    pub render_type: i32,
}

impl ClientboundPacket for SetObjective {
    const NAME: &'static str = "set_objective";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.name);
        match self.mode {
            ObjectiveMode::Create => w.write_u8(0),
            ObjectiveMode::Remove => {
                w.write_u8(1);
                return;
            }
            ObjectiveMode::Update => w.write_u8(2),
        }
        w.write_text(&self.display_name);
        w.write_varint(self.render_type);
        w.write_bool(false); // no number format
    }
}

pub struct SetScore {
    pub owner: String,
    pub objective: String,
    pub value: i32,
}

impl ClientboundPacket for SetScore {
    const NAME: &'static str = "set_score";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.owner);
        w.write_string(&self.objective);
        w.write_varint(self.value);
        w.write_bool(false); // no display name
        w.write_bool(false); // no number format
    }
}

pub struct ResetScore {
    pub owner: String,
    /// `None` resets every objective.
    pub objective: Option<String>,
}

impl ClientboundPacket for ResetScore {
    const NAME: &'static str = "reset_score";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.owner);
        w.write_option(self.objective.as_ref(), |w, o| w.write_string(o));
    }
}

/// 0 = player list, 1 = sidebar, 2 = below name, 3.. = sidebar for a team colour.
pub struct SetDisplayObjective {
    pub position: i32,
    pub objective: String,
}

impl ClientboundPacket for SetDisplayObjective {
    const NAME: &'static str = "set_display_objective";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.position);
        w.write_string(&self.objective);
    }
}

// ---- teams ----

#[derive(Clone, Debug)]
pub struct TeamParameters {
    pub display_name: Text,
    pub prefix: Text,
    pub suffix: Text,
    /// 0 always, 1 never, 2 hide for other teams, 3 hide for own team.
    pub name_tag_visibility: i32,
    /// 0 always, 1 never, 2 push other teams, 3 push own team.
    pub collision_rule: i32,
    /// A formatting colour id (0-15), or `None` for reset.
    pub color: Option<i32>,
    /// bit 0 friendly fire, bit 1 see invisible friends.
    pub options: u8,
}

impl TeamParameters {
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(&self.display_name);
        w.write_text(&self.prefix);
        w.write_text(&self.suffix);
        w.write_varint(self.name_tag_visibility);
        w.write_varint(self.collision_rule);
        w.write_option(self.color.as_ref(), |w, c| w.write_varint(*c));
        w.write_u8(self.options);
    }
}

pub enum TeamMethod {
    Create { parameters: TeamParameters, players: Vec<String> },
    Remove,
    Update(TeamParameters),
    AddPlayers(Vec<String>),
    RemovePlayers(Vec<String>),
}

pub struct SetPlayerTeam {
    pub name: String,
    pub method: TeamMethod,
}

impl ClientboundPacket for SetPlayerTeam {
    const NAME: &'static str = "set_player_team";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(&self.name);
        match &self.method {
            TeamMethod::Create { parameters, players } => {
                w.write_u8(0);
                parameters.write(w);
                w.write_list(players, |w, p| w.write_string(p));
            }
            TeamMethod::Remove => w.write_u8(1),
            TeamMethod::Update(parameters) => {
                w.write_u8(2);
                parameters.write(w);
            }
            TeamMethod::AddPlayers(players) => {
                w.write_u8(3);
                w.write_list(players, |w, p| w.write_string(p));
            }
            TeamMethod::RemovePlayers(players) => {
                w.write_u8(4);
                w.write_list(players, |w, p| w.write_string(p));
            }
        }
    }
}

// ---- effects and attributes ----

pub struct UpdateMobEffect {
    pub entity_id: i32,
    /// Index into `minecraft:mob_effect`.
    pub effect: i32,
    pub amplifier: i32,
    /// Ticks; -1 is infinite.
    pub duration: i32,
    pub ambient: bool,
    pub show_particles: bool,
    pub show_icon: bool,
}

impl ClientboundPacket for UpdateMobEffect {
    const NAME: &'static str = "update_mob_effect";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_varint(self.effect);
        w.write_varint(self.amplifier);
        w.write_varint(self.duration);
        let flags = (self.ambient as u8) | ((self.show_particles as u8) << 1) | ((self.show_icon as u8) << 2);
        w.write_u8(flags);
    }
}

pub struct RemoveMobEffect {
    pub entity_id: i32,
    pub effect: i32,
}

impl ClientboundPacket for RemoveMobEffect {
    const NAME: &'static str = "remove_mob_effect";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_varint(self.effect);
    }
}

pub struct AttributeModifier {
    pub id: Identifier,
    pub amount: f64,
    /// 0 add value, 1 add multiplied base, 2 add multiplied total.
    pub operation: u8,
}

pub struct AttributeSnapshot {
    /// Index into `minecraft:attribute`.
    pub attribute: i32,
    pub base: f64,
    pub modifiers: Vec<AttributeModifier>,
}

pub struct UpdateAttributes {
    pub entity_id: i32,
    pub attributes: Vec<AttributeSnapshot>,
}

impl ClientboundPacket for UpdateAttributes {
    const NAME: &'static str = "update_attributes";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_list(&self.attributes, |w, a| {
            w.write_varint(a.attribute);
            w.write_f64(a.base);
            w.write_list(&a.modifiers, |w, m| {
                w.write_identifier(&m.id);
                w.write_f64(m.amount);
                w.write_u8(m.operation);
            });
        });
    }
}

pub struct SetCamera {
    pub entity_id: i32,
}

impl ClientboundPacket for SetCamera {
    const NAME: &'static str = "set_camera";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
    }
}

/// Game rule values the client should know about (they affect prediction
/// and some visuals). Keys are `minecraft:game_rule` ids.
pub struct GameRuleValues {
    pub values: Vec<(Identifier, String)>,
}

impl ClientboundPacket for GameRuleValues {
    const NAME: &'static str = "game_rule_values";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_list(&self.values, |w, (key, value)| {
            w.write_identifier(key);
            w.write_string(value);
        });
    }
}

/// Who rides what. An empty list means everyone got off.
pub struct SetPassengers {
    pub vehicle: i32,
    pub passengers: Vec<i32>,
}

impl ClientboundPacket for SetPassengers {
    const NAME: &'static str = "set_passengers";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.vehicle);
        w.write_list(&self.passengers, |w, p| w.write_varint(*p));
    }
}

/// The pickup animation: an item flies into a player.
pub struct TakeItemEntity {
    pub item_id: i32,
    pub player_id: i32,
    pub amount: i32,
}

impl ClientboundPacket for TakeItemEntity {
    const NAME: &'static str = "take_item_entity";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.item_id);
        w.write_varint(self.player_id);
        w.write_varint(self.amount);
    }
}

pub struct PlayerCombatKill {
    pub player_id: i32,
    pub message: Text,
}

impl ClientboundPacket for PlayerCombatKill {
    const NAME: &'static str = "player_combat_kill";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.player_id);
        w.write_text(&self.message);
    }
}

pub struct HurtAnimation {
    pub entity_id: i32,
    pub yaw: f32,
}

impl ClientboundPacket for HurtAnimation {
    const NAME: &'static str = "hurt_animation";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_f32(self.yaw);
    }
}

pub struct DamageEvent {
    pub entity_id: i32,
    /// Index into `minecraft:damage_type`.
    pub damage_type: i32,
}

impl ClientboundPacket for DamageEvent {
    const NAME: &'static str = "damage_event";
    const STATE: State = State::Play;
    fn write(&self, w: &mut PacketWriter) {
        w.write_varint(self.entity_id);
        w.write_varint(self.damage_type);
        w.write_varint(0); // no cause entity
        w.write_varint(0); // no direct entity
        w.write_bool(false); // no source position
    }
}
