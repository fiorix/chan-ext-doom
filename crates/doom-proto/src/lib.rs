//! Byte-exact codecs for the Chocolate Doom 3.1.1 network protocol.
//!
//! Two directional packet families, because the same type number has
//! different layouts depending on direction: [`ClientPacket`] is what a
//! client sends (c2s), [`ServerPacket`] is what a server sends (s2c).
//! Direction is therefore part of the type, not a flag the caller can
//! forget.
//!
//! Everything is explicit big-endian fixed-width: no transmute, no C
//! layout, no unsafe, no I/O, no async. The layouts are the observed
//! and reference-grounded wire facts recorded in `docs/protocol.md`;
//! volatile fields (garbage `player_class`, random pet names) are
//! preserved verbatim so a decoded packet re-encodes to the exact
//! original bytes.

#![forbid(unsafe_code)]

/// Magic value carried by a client `SYN` (`NET_MAGIC_NUMBER`).
pub const NET_MAGIC: u32 = 0x56ab_e18c;

/// Bit 15 of the type word marking a reliable packet.
pub const RELIABLE_BIT: u16 = 0x8000;

/// `NET_MAXPLAYERS`: the net layer's player ceiling.
pub const NET_MAXPLAYERS: usize = 8;

/// `MAXPLAYERNAME`: server-side name/address string limit (with NUL).
pub const MAX_PLAYER_NAME: usize = 30;

/// The per-packet framing above the type word: whether the packet is
/// reliable and, if so, its per-direction sequence byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireHeader {
    /// `Some(seq)` for a reliable packet, `None` for a plain one.
    pub reliable_seq: Option<u8>,
}

/// Ways a byte string can fail to decode into a packet. Carries no
/// attacker-controlled data and no allocations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Fewer bytes remain than the named field requires.
    Truncated { field: &'static str },
    /// The layout ended before the datagram did.
    TrailingBytes { trailing: usize },
    /// The SYN magic value was not `NET_MAGIC`.
    BadMagic { got: u32 },
    /// A type number with no supported layout: deprecated ACK,
    /// NAT_HOLE_PUNCH, out of range, or wrong family for the type.
    UnsupportedType { type_id: u16 },
    /// No NUL terminator before the end of the packet.
    UnterminatedString { field: &'static str },
    /// A string that must be printable was not.
    UnprintableString { field: &'static str },
    /// A bounded string field exceeded its limit.
    StringTooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
    /// A count exceeded its protocol maximum.
    CountOutOfRange {
        field: &'static str,
        count: usize,
        max: usize,
    },
    /// A field value the protocol does not allow.
    InvalidValue { field: &'static str },
}

/// Ways a value can fail to encode. Constructors accept whatever the
/// caller builds, so these are checked at encode time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// A string containing an interior NUL cannot go on the wire.
    InteriorNul { field: &'static str },
    /// A string that must be printable was not.
    UnprintableString { field: &'static str },
    /// A bounded string field exceeded its limit.
    StringTooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
    /// A repeated field exceeded its protocol maximum.
    CountOutOfRange {
        field: &'static str,
        count: usize,
        max: usize,
    },
}

fn printable(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|&b| (0x20..=0x7e).contains(&b) || b == b'\n')
}

// --- wire reader ---------------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn take(&mut self, n: usize, field: &'static str) -> Result<&'a [u8], DecodeError> {
        if self.buf.len() - self.pos < n {
            return Err(DecodeError::Truncated { field });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, DecodeError> {
        Ok(self.take(1, field)?[0])
    }

    fn s8(&mut self, field: &'static str) -> Result<i8, DecodeError> {
        Ok(self.u8(field)? as i8)
    }

    fn u16(&mut self, field: &'static str) -> Result<u16, DecodeError> {
        let b = self.take(2, field)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn s16(&mut self, field: &'static str) -> Result<i16, DecodeError> {
        Ok(self.u16(field)? as i16)
    }

    fn u32(&mut self, field: &'static str) -> Result<u32, DecodeError> {
        let b = self.take(4, field)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn sha1(&mut self, field: &'static str) -> Result<[u8; 20], DecodeError> {
        let b = self.take(20, field)?;
        let mut out = [0u8; 20];
        out.copy_from_slice(b);
        Ok(out)
    }

    /// A NUL-terminated string. `max` counts content bytes, excluding
    /// the terminator (names use `MAX_PLAYER_NAME - 1`).
    fn nul_string(
        &mut self,
        field: &'static str,
        max: Option<usize>,
        require_printable: bool,
    ) -> Result<&'a [u8], DecodeError> {
        let start = self.pos;
        let mut end = start;
        while end < self.buf.len() && self.buf[end] != 0 {
            end += 1;
        }
        if end == self.buf.len() {
            return Err(DecodeError::UnterminatedString { field });
        }
        let s = &self.buf[start..end];
        self.pos = end + 1;
        if let Some(max) = max
            && s.len() > max
        {
            return Err(DecodeError::StringTooLong {
                field,
                len: s.len(),
                max,
            });
        }
        if require_printable && !printable(s) {
            return Err(DecodeError::UnprintableString { field });
        }
        Ok(s)
    }

    fn finish(self) -> Result<(), DecodeError> {
        let trailing = self.buf.len() - self.pos;
        if trailing > 0 {
            return Err(DecodeError::TrailingBytes { trailing });
        }
        Ok(())
    }
}

// --- wire writer ---------------------------------------------------------

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn s8(&mut self, v: i8) {
        self.u8(v as u8);
    }

    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    fn s16(&mut self, v: i16) {
        self.u16(v as u16);
    }

    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn nul_string(
        &mut self,
        field: &'static str,
        s: &[u8],
        max: Option<usize>,
        require_printable: bool,
    ) -> Result<(), EncodeError> {
        if s.contains(&0) {
            return Err(EncodeError::InteriorNul { field });
        }
        if let Some(max) = max
            && s.len() > max
        {
            return Err(EncodeError::StringTooLong {
                field,
                len: s.len(),
                max,
            });
        }
        if require_printable && !printable(s) {
            return Err(EncodeError::UnprintableString { field });
        }
        self.raw(s);
        self.u8(0);
        Ok(())
    }
}

// --- shared value types --------------------------------------------------

/// The connect data of a client `SYN` (`net_connect_data_t`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectData {
    pub gamemode: u8,
    pub gamemission: u8,
    pub lowres_turn: u8,
    pub drone: u8,
    pub max_players: u8,
    pub is_freedoom: u8,
    pub wad_sha1: [u8; 20],
    pub deh_sha1: [u8; 20],
    pub player_class: u8,
}

impl ConnectData {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let data = ConnectData {
            gamemode: r.u8("connect.gamemode")?,
            gamemission: r.u8("connect.gamemission")?,
            lowres_turn: r.u8("connect.lowres_turn")?,
            drone: r.u8("connect.drone")?,
            max_players: r.u8("connect.max_players")?,
            is_freedoom: r.u8("connect.is_freedoom")?,
            wad_sha1: r.sha1("connect.wad_sha1")?,
            deh_sha1: r.sha1("connect.deh_sha1")?,
            player_class: r.u8("connect.player_class")?,
        };
        if data.max_players as usize > NET_MAXPLAYERS {
            return Err(DecodeError::CountOutOfRange {
                field: "connect.max_players",
                count: data.max_players as usize,
                max: NET_MAXPLAYERS,
            });
        }
        Ok(data)
    }

    fn encode(&self, w: &mut Writer) {
        w.u8(self.gamemode);
        w.u8(self.gamemission);
        w.u8(self.lowres_turn);
        w.u8(self.drone);
        w.u8(self.max_players);
        w.u8(self.is_freedoom);
        w.raw(&self.wad_sha1);
        w.raw(&self.deh_sha1);
        w.u8(self.player_class);
    }
}

/// A client `SYN` (type 0 c2s): magic, version, protocol list, connect
/// data, player name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Syn {
    pub version: Vec<u8>,
    pub protocols: Vec<Vec<u8>>,
    pub connect: ConnectData,
    pub player_name: Vec<u8>,
}

/// The server `SYN` accept (type 0 s2c, reliable): version and the one
/// negotiated protocol name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SynAccept {
    pub version: Vec<u8>,
    pub protocol: Vec<u8>,
}

/// One player entry in a `WAITING_DATA` lobby update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaitPlayer {
    pub name: Vec<u8>,
    pub addr: Vec<u8>,
}

/// The lobby state broadcast (`net_waitdata_t`, type 4 s2c). The wire
/// player count is `players.len()`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaitData {
    pub players: Vec<WaitPlayer>,
    pub num_drones: u8,
    pub ready_players: u8,
    pub max_players: u8,
    pub is_controller: u8,
    pub consoleplayer: i8,
    pub wad_sha1: [u8; 20],
    pub deh_sha1: [u8; 20],
    pub is_freedoom: u8,
}

/// The game settings carried by `GAMESTART` in both directions
/// (`net_gamesettings_t`). The wire `num_players` is
/// `player_classes.len()`; `consoleplayer` is signed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameSettings {
    pub ticdup: u8,
    pub extratics: u8,
    pub deathmatch: u8,
    pub nomonsters: u8,
    pub fast_monsters: u8,
    pub respawn_monsters: u8,
    pub episode: u8,
    pub map: u8,
    pub skill: i8,
    pub gameversion: u8,
    pub lowres_turn: u8,
    pub new_sync: u8,
    pub timelimit: u32,
    pub loadgame: i8,
    pub random: u8,
    pub consoleplayer: i8,
    pub player_classes: Vec<u8>,
}

impl GameSettings {
    fn decode(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let mut s = GameSettings {
            ticdup: r.u8("settings.ticdup")?,
            extratics: r.u8("settings.extratics")?,
            deathmatch: r.u8("settings.deathmatch")?,
            nomonsters: r.u8("settings.nomonsters")?,
            fast_monsters: r.u8("settings.fast_monsters")?,
            respawn_monsters: r.u8("settings.respawn_monsters")?,
            episode: r.u8("settings.episode")?,
            map: r.u8("settings.map")?,
            skill: r.s8("settings.skill")?,
            gameversion: r.u8("settings.gameversion")?,
            lowres_turn: r.u8("settings.lowres_turn")?,
            new_sync: r.u8("settings.new_sync")?,
            timelimit: r.u32("settings.timelimit")?,
            loadgame: r.s8("settings.loadgame")?,
            random: r.u8("settings.random")?,
            consoleplayer: 0,
            player_classes: Vec::new(),
        };
        let num_players = r.u8("settings.num_players")? as usize;
        if num_players > NET_MAXPLAYERS {
            return Err(DecodeError::CountOutOfRange {
                field: "settings.num_players",
                count: num_players,
                max: NET_MAXPLAYERS,
            });
        }
        s.consoleplayer = r.s8("settings.consoleplayer")?;
        for _ in 0..num_players {
            s.player_classes.push(r.u8("settings.player_classes")?);
        }
        Ok(s)
    }

    fn encode(&self, w: &mut Writer) -> Result<(), EncodeError> {
        if self.player_classes.len() > NET_MAXPLAYERS {
            return Err(EncodeError::CountOutOfRange {
                field: "settings.player_classes",
                count: self.player_classes.len(),
                max: NET_MAXPLAYERS,
            });
        }
        w.u8(self.ticdup);
        w.u8(self.extratics);
        w.u8(self.deathmatch);
        w.u8(self.nomonsters);
        w.u8(self.fast_monsters);
        w.u8(self.respawn_monsters);
        w.u8(self.episode);
        w.u8(self.map);
        w.s8(self.skill);
        w.u8(self.gameversion);
        w.u8(self.lowres_turn);
        w.u8(self.new_sync);
        w.u32(self.timelimit);
        w.s8(self.loadgame);
        w.u8(self.random);
        w.u8(self.player_classes.len() as u8);
        w.s8(self.consoleplayer);
        for &class in &self.player_classes {
            w.u8(class);
        }
        Ok(())
    }
}

/// One player's command delta inside `GAMEDATA`. Field presence is the
/// wire bitmask exactly: `Some` means the bit was set and the field is
/// present, `None` means absent. `turn` is stored expanded (lowres
/// turn arrives as s8 times 256 and is written back as s8 on encode,
/// so the round trip is exact).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TiccmdDiff {
    pub forward: Option<i8>,
    pub side: Option<i8>,
    pub turn: Option<i16>,
    pub buttons: Option<u8>,
    pub consistancy: Option<u8>,
    pub chatchar: Option<u8>,
    pub lookfly: Option<u8>,
    pub arti: Option<u8>,
    pub buttons2: Option<u8>,
    pub inventory: Option<u16>,
}

const DIFF_FORWARD: u8 = 1 << 0;
const DIFF_SIDE: u8 = 1 << 1;
const DIFF_TURN: u8 = 1 << 2;
const DIFF_BUTTONS: u8 = 1 << 3;
const DIFF_CONSISTANCY: u8 = 1 << 4;
const DIFF_CHATCHAR: u8 = 1 << 5;
const DIFF_RAVEN: u8 = 1 << 6;
const DIFF_STRIFE: u8 = 1 << 7;

impl TiccmdDiff {
    fn decode(r: &mut Reader<'_>, lowres_turn: bool) -> Result<Self, DecodeError> {
        let mask = r.u8("ticdiff.mask")?;
        let mut d = TiccmdDiff::default();
        if mask & DIFF_FORWARD != 0 {
            d.forward = Some(r.s8("ticdiff.forward")?);
        }
        if mask & DIFF_SIDE != 0 {
            d.side = Some(r.s8("ticdiff.side")?);
        }
        if mask & DIFF_TURN != 0 {
            d.turn = Some(if lowres_turn {
                i16::from(r.s8("ticdiff.turn_lowres")?) * 256
            } else {
                r.s16("ticdiff.turn")?
            });
        }
        if mask & DIFF_BUTTONS != 0 {
            d.buttons = Some(r.u8("ticdiff.buttons")?);
        }
        if mask & DIFF_CONSISTANCY != 0 {
            d.consistancy = Some(r.u8("ticdiff.consistancy")?);
        }
        if mask & DIFF_CHATCHAR != 0 {
            d.chatchar = Some(r.u8("ticdiff.chatchar")?);
        }
        if mask & DIFF_RAVEN != 0 {
            d.lookfly = Some(r.u8("ticdiff.lookfly")?);
            d.arti = Some(r.u8("ticdiff.arti")?);
        }
        if mask & DIFF_STRIFE != 0 {
            d.buttons2 = Some(r.u8("ticdiff.buttons2")?);
            d.inventory = Some(r.u16("ticdiff.inventory")?);
        }
        Ok(d)
    }

    fn encode(&self, w: &mut Writer, lowres_turn: bool) {
        let mut mask = 0u8;
        if self.forward.is_some() {
            mask |= DIFF_FORWARD;
        }
        if self.side.is_some() {
            mask |= DIFF_SIDE;
        }
        if self.turn.is_some() {
            mask |= DIFF_TURN;
        }
        if self.buttons.is_some() {
            mask |= DIFF_BUTTONS;
        }
        if self.consistancy.is_some() {
            mask |= DIFF_CONSISTANCY;
        }
        if self.chatchar.is_some() {
            mask |= DIFF_CHATCHAR;
        }
        if self.lookfly.is_some() || self.arti.is_some() {
            mask |= DIFF_RAVEN;
        }
        if self.buttons2.is_some() || self.inventory.is_some() {
            mask |= DIFF_STRIFE;
        }
        w.u8(mask);
        if let Some(v) = self.forward {
            w.s8(v);
        }
        if let Some(v) = self.side {
            w.s8(v);
        }
        if let Some(v) = self.turn {
            if lowres_turn {
                w.s8((v / 256) as i8);
            } else {
                w.s16(v);
            }
        }
        if let Some(v) = self.buttons {
            w.u8(v);
        }
        if let Some(v) = self.consistancy {
            w.u8(v);
        }
        if let Some(v) = self.chatchar {
            w.u8(v);
        }
        if self.lookfly.is_some() || self.arti.is_some() {
            w.u8(self.lookfly.unwrap_or(0));
            w.u8(self.arti.unwrap_or(0));
        }
        if self.buttons2.is_some() || self.inventory.is_some() {
            w.u8(self.buttons2.unwrap_or(0));
            w.u16(self.inventory.unwrap_or(0));
        }
    }
}

/// One tic of a client's `GAMEDATA` upload: latency plus its diff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientTic {
    pub latency: i16,
    pub diff: TiccmdDiff,
}

/// The client `GAMEDATA` upload (type 6 c2s): window ack, start tic,
/// then `tics.len()` tics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameDataClient {
    pub ack: u8,
    pub start: u8,
    pub tics: Vec<ClientTic>,
}

/// One tic of the server's `GAMEDATA` fan-out: latency and the active
/// players' diffs in ascending player order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FullTic {
    pub latency: i16,
    /// `(player index, diff)` pairs, strictly ascending by index,
    /// matching the wire playeringame bitmask.
    pub players: Vec<(u8, TiccmdDiff)>,
}

/// The server `GAMEDATA` fan-out (type 6 s2c): start tic, then
/// `tics.len()` full tic records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GameDataServer {
    pub start: u8,
    pub tics: Vec<FullTic>,
}

/// The server query response (type 14 s2c). `protocols` is `Some` when
/// the wire carried a protocol list (even an empty one) and `None`
/// when it ended after the description.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryData {
    pub version: Vec<u8>,
    pub server_state: u8,
    pub num_players: u8,
    pub max_players: u8,
    pub gamemode: u8,
    pub gamemission: u8,
    pub description: Vec<u8>,
    pub protocols: Option<Vec<Vec<u8>>>,
}

// --- packet families -----------------------------------------------------

fn split_header(r: &mut Reader<'_>) -> Result<(u16, WireHeader), DecodeError> {
    let word = r.u16("header.type")?;
    let reliable = word & RELIABLE_BIT != 0;
    let reliable_seq = if reliable {
        Some(r.u8("header.reliable_seq")?)
    } else {
        None
    };
    Ok((word & !RELIABLE_BIT, WireHeader { reliable_seq }))
}

/// Client-to-server packets (c2s).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientPacket {
    Syn(Syn),
    Keepalive,
    /// Type 15 c2s: reliable, empty body.
    Launch,
    GameStart(GameSettings),
    GameData(GameDataClient),
    GameDataAck {
        ack: u8,
    },
    Disconnect,
    DisconnectAck,
    ReliableAck {
        next_seq: u8,
    },
    GameDataResend {
        start: u32,
        count: u8,
    },
    /// Type 13 c2s: empty body.
    Query,
}

impl ClientPacket {
    /// The wire type number of this packet.
    pub fn type_id(&self) -> u16 {
        match self {
            ClientPacket::Syn(_) => 0,
            ClientPacket::Keepalive => 3,
            ClientPacket::Launch => 15,
            ClientPacket::GameStart(_) => 5,
            ClientPacket::GameData(_) => 6,
            ClientPacket::GameDataAck { .. } => 7,
            ClientPacket::Disconnect => 8,
            ClientPacket::DisconnectAck => 9,
            ClientPacket::ReliableAck { .. } => 10,
            ClientPacket::GameDataResend { .. } => 11,
            ClientPacket::Query => 13,
        }
    }

    /// Decode one datagram as a client packet. `lowres_turn` selects
    /// the narrow `angleturn` form inside `GAMEDATA` (a session
    /// property agreed in GAMESTART).
    pub fn decode(bytes: &[u8], lowres_turn: bool) -> Result<(WireHeader, Self), DecodeError> {
        let mut r = Reader::new(bytes);
        let (type_id, header) = split_header(&mut r)?;
        let packet = match type_id {
            0 => {
                let magic = r.u32("syn.magic")?;
                if magic != NET_MAGIC {
                    return Err(DecodeError::BadMagic { got: magic });
                }
                let version = r.nul_string("syn.version", None, true)?.to_vec();
                let num_protocols = r.u8("syn.num_protocols")? as usize;
                if num_protocols == 0 || num_protocols > 16 {
                    return Err(DecodeError::CountOutOfRange {
                        field: "syn.num_protocols",
                        count: num_protocols,
                        max: 16,
                    });
                }
                let mut protocols = Vec::with_capacity(num_protocols);
                for _ in 0..num_protocols {
                    protocols.push(r.nul_string("syn.protocol", None, true)?.to_vec());
                }
                let connect = ConnectData::decode(&mut r)?;
                let player_name = r
                    .nul_string("syn.player_name", Some(MAX_PLAYER_NAME - 1), false)?
                    .to_vec();
                ClientPacket::Syn(Syn {
                    version,
                    protocols,
                    connect,
                    player_name,
                })
            }
            3 => ClientPacket::Keepalive,
            5 => ClientPacket::GameStart(GameSettings::decode(&mut r)?),
            6 => {
                let ack = r.u8("gamedata.ack")?;
                let start = r.u8("gamedata.start")?;
                let count = r.u8("gamedata.count")? as usize;
                let mut tics = Vec::with_capacity(count);
                for _ in 0..count {
                    let latency = r.s16("gamedata.latency")?;
                    let diff = TiccmdDiff::decode(&mut r, lowres_turn)?;
                    tics.push(ClientTic { latency, diff });
                }
                ClientPacket::GameData(GameDataClient { ack, start, tics })
            }
            7 => ClientPacket::GameDataAck {
                ack: r.u8("gamedata_ack.ack")?,
            },
            8 => ClientPacket::Disconnect,
            9 => ClientPacket::DisconnectAck,
            10 => ClientPacket::ReliableAck {
                next_seq: r.u8("reliable_ack.next_seq")?,
            },
            11 => ClientPacket::GameDataResend {
                start: r.u32("resend.start")?,
                count: r.u8("resend.count")?,
            },
            15 => ClientPacket::Launch,
            13 => ClientPacket::Query,
            other => return Err(DecodeError::UnsupportedType { type_id: other }),
        };
        r.finish()?;
        Ok((header, packet))
    }

    /// Encode to exact wire bytes with the given framing. `lowres_turn`
    /// must match the session's GAMESTART setting for `GAMEDATA`.
    pub fn encode(&self, header: WireHeader, lowres_turn: bool) -> Result<Vec<u8>, EncodeError> {
        let mut w = Writer::new();
        w.u16(
            self.type_id()
                | if header.reliable_seq.is_some() {
                    RELIABLE_BIT
                } else {
                    0
                },
        );
        if let Some(seq) = header.reliable_seq {
            w.u8(seq);
        }
        match self {
            ClientPacket::Syn(syn) => {
                w.u32(NET_MAGIC);
                w.nul_string("syn.version", &syn.version, None, true)?;
                if syn.protocols.is_empty() || syn.protocols.len() > 16 {
                    return Err(EncodeError::CountOutOfRange {
                        field: "syn.protocols",
                        count: syn.protocols.len(),
                        max: 16,
                    });
                }
                w.u8(syn.protocols.len() as u8);
                for protocol in &syn.protocols {
                    w.nul_string("syn.protocol", protocol, None, true)?;
                }
                if syn.connect.max_players as usize > NET_MAXPLAYERS {
                    return Err(EncodeError::CountOutOfRange {
                        field: "connect.max_players",
                        count: syn.connect.max_players as usize,
                        max: NET_MAXPLAYERS,
                    });
                }
                syn.connect.encode(&mut w);
                w.nul_string(
                    "syn.player_name",
                    &syn.player_name,
                    Some(MAX_PLAYER_NAME - 1),
                    false,
                )?;
            }
            ClientPacket::Keepalive
            | ClientPacket::Launch
            | ClientPacket::Disconnect
            | ClientPacket::DisconnectAck
            | ClientPacket::Query => {}
            ClientPacket::GameStart(settings) => settings.encode(&mut w)?,
            ClientPacket::GameData(data) => {
                w.u8(data.ack);
                w.u8(data.start);
                w.u8(data.tics.len() as u8);
                for tic in &data.tics {
                    w.s16(tic.latency);
                    tic.diff.encode(&mut w, lowres_turn);
                }
            }
            ClientPacket::GameDataAck { ack } => w.u8(*ack),
            ClientPacket::ReliableAck { next_seq } => w.u8(*next_seq),
            ClientPacket::GameDataResend { start, count } => {
                w.u32(*start);
                w.u8(*count);
            }
        }
        Ok(w.buf)
    }
}

/// Server-to-client packets (s2c).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerPacket {
    SynAccept(SynAccept),
    Rejected { reason: Vec<u8> },
    Keepalive,
    WaitingData(WaitData),
    Launch { num_players: u8 },
    GameStart(GameSettings),
    GameData(GameDataServer),
    Disconnect,
    DisconnectAck,
    ReliableAck { next_seq: u8 },
    GameDataResend { start: u32, count: u8 },
    ConsoleMessage { message: Vec<u8> },
    QueryResponse(QueryData),
}

impl ServerPacket {
    /// The wire type number of this packet.
    pub fn type_id(&self) -> u16 {
        match self {
            ServerPacket::SynAccept(_) => 0,
            ServerPacket::Rejected { .. } => 2,
            ServerPacket::Keepalive => 3,
            ServerPacket::WaitingData(_) => 4,
            ServerPacket::Launch { .. } => 15,
            ServerPacket::GameStart(_) => 5,
            ServerPacket::GameData(_) => 6,
            ServerPacket::Disconnect => 8,
            ServerPacket::DisconnectAck => 9,
            ServerPacket::ReliableAck { .. } => 10,
            ServerPacket::GameDataResend { .. } => 11,
            ServerPacket::ConsoleMessage { .. } => 12,
            ServerPacket::QueryResponse(_) => 14,
        }
    }

    /// Decode one datagram as a server packet. `lowres_turn` selects
    /// the narrow `angleturn` form inside `GAMEDATA`.
    pub fn decode(bytes: &[u8], lowres_turn: bool) -> Result<(WireHeader, Self), DecodeError> {
        let mut r = Reader::new(bytes);
        let (type_id, header) = split_header(&mut r)?;
        let packet = match type_id {
            0 => {
                let version = r.nul_string("syn_accept.version", None, true)?.to_vec();
                let protocol = r.nul_string("syn_accept.protocol", None, true)?.to_vec();
                ServerPacket::SynAccept(SynAccept { version, protocol })
            }
            2 => ServerPacket::Rejected {
                reason: r.nul_string("rejected.reason", None, true)?.to_vec(),
            },
            3 => ServerPacket::Keepalive,
            4 => {
                let num_players = r.u8("waitdata.num_players")? as usize;
                if num_players > NET_MAXPLAYERS {
                    return Err(DecodeError::CountOutOfRange {
                        field: "waitdata.num_players",
                        count: num_players,
                        max: NET_MAXPLAYERS,
                    });
                }
                let num_drones = r.u8("waitdata.num_drones")?;
                let ready_players = r.u8("waitdata.ready_players")?;
                let max_players = r.u8("waitdata.max_players")?;
                let is_controller = r.u8("waitdata.is_controller")?;
                let consoleplayer = r.s8("waitdata.consoleplayer")?;
                let mut players = Vec::with_capacity(num_players);
                for _ in 0..num_players {
                    let name = r
                        .nul_string("waitdata.player_name", Some(MAX_PLAYER_NAME - 1), false)?
                        .to_vec();
                    let addr = r
                        .nul_string("waitdata.player_addr", Some(MAX_PLAYER_NAME - 1), false)?
                        .to_vec();
                    players.push(WaitPlayer { name, addr });
                }
                let wad_sha1 = r.sha1("waitdata.wad_sha1")?;
                let deh_sha1 = r.sha1("waitdata.deh_sha1")?;
                let is_freedoom = r.u8("waitdata.is_freedoom")?;
                ServerPacket::WaitingData(WaitData {
                    players,
                    num_drones,
                    ready_players,
                    max_players,
                    is_controller,
                    consoleplayer,
                    wad_sha1,
                    deh_sha1,
                    is_freedoom,
                })
            }
            5 => ServerPacket::GameStart(GameSettings::decode(&mut r)?),
            6 => {
                let start = r.u8("gamedata.start")?;
                let count = r.u8("gamedata.count")? as usize;
                let mut tics = Vec::with_capacity(count);
                for _ in 0..count {
                    let latency = r.s16("gamedata.latency")?;
                    let mask = r.u8("gamedata.playeringame")?;
                    let mut players = Vec::new();
                    for index in 0..NET_MAXPLAYERS as u8 {
                        if mask & (1 << index) != 0 {
                            let diff = TiccmdDiff::decode(&mut r, lowres_turn)?;
                            players.push((index, diff));
                        }
                    }
                    tics.push(FullTic { latency, players });
                }
                ServerPacket::GameData(GameDataServer { start, tics })
            }
            8 => ServerPacket::Disconnect,
            9 => ServerPacket::DisconnectAck,
            10 => ServerPacket::ReliableAck {
                next_seq: r.u8("reliable_ack.next_seq")?,
            },
            11 => ServerPacket::GameDataResend {
                start: r.u32("resend.start")?,
                count: r.u8("resend.count")?,
            },
            12 => ServerPacket::ConsoleMessage {
                message: r.nul_string("console.message", None, true)?.to_vec(),
            },
            14 => {
                let version = r.nul_string("query.version", None, true)?.to_vec();
                let server_state = r.u8("query.server_state")?;
                let num_players = r.u8("query.num_players")?;
                let max_players = r.u8("query.max_players")?;
                let gamemode = r.u8("query.gamemode")?;
                let gamemission = r.u8("query.gamemission")?;
                let description = r.nul_string("query.description", None, true)?.to_vec();
                // Old servers end here; a present list, even empty, is kept.
                let protocols = if r.pos < bytes.len() {
                    let num_protocols = r.u8("query.num_protocols")? as usize;
                    if num_protocols > 16 {
                        return Err(DecodeError::CountOutOfRange {
                            field: "query.num_protocols",
                            count: num_protocols,
                            max: 16,
                        });
                    }
                    let mut list = Vec::with_capacity(num_protocols);
                    for _ in 0..num_protocols {
                        list.push(r.nul_string("query.protocol", None, true)?.to_vec());
                    }
                    Some(list)
                } else {
                    None
                };
                ServerPacket::QueryResponse(QueryData {
                    version,
                    server_state,
                    num_players,
                    max_players,
                    gamemode,
                    gamemission,
                    description,
                    protocols,
                })
            }
            15 => ServerPacket::Launch {
                num_players: r.u8("launch.num_players")?,
            },
            other => return Err(DecodeError::UnsupportedType { type_id: other }),
        };
        r.finish()?;
        Ok((header, packet))
    }

    /// Encode to exact wire bytes with the given framing. `lowres_turn`
    /// must match the session's GAMESTART setting for `GAMEDATA`.
    pub fn encode(&self, header: WireHeader, lowres_turn: bool) -> Result<Vec<u8>, EncodeError> {
        let mut w = Writer::new();
        w.u16(
            self.type_id()
                | if header.reliable_seq.is_some() {
                    RELIABLE_BIT
                } else {
                    0
                },
        );
        if let Some(seq) = header.reliable_seq {
            w.u8(seq);
        }
        match self {
            ServerPacket::SynAccept(accept) => {
                w.nul_string("syn_accept.version", &accept.version, None, true)?;
                w.nul_string("syn_accept.protocol", &accept.protocol, None, true)?;
            }
            ServerPacket::Rejected { reason } => {
                w.nul_string("rejected.reason", reason, None, true)?;
            }
            ServerPacket::Keepalive | ServerPacket::Disconnect | ServerPacket::DisconnectAck => {}
            ServerPacket::WaitingData(data) => {
                if data.players.len() > NET_MAXPLAYERS {
                    return Err(EncodeError::CountOutOfRange {
                        field: "waitdata.players",
                        count: data.players.len(),
                        max: NET_MAXPLAYERS,
                    });
                }
                w.u8(data.players.len() as u8);
                w.u8(data.num_drones);
                w.u8(data.ready_players);
                w.u8(data.max_players);
                w.u8(data.is_controller);
                w.s8(data.consoleplayer);
                for player in &data.players {
                    w.nul_string(
                        "waitdata.player_name",
                        &player.name,
                        Some(MAX_PLAYER_NAME - 1),
                        false,
                    )?;
                    w.nul_string(
                        "waitdata.player_addr",
                        &player.addr,
                        Some(MAX_PLAYER_NAME - 1),
                        false,
                    )?;
                }
                w.raw(&data.wad_sha1);
                w.raw(&data.deh_sha1);
                w.u8(data.is_freedoom);
            }
            ServerPacket::GameStart(settings) => settings.encode(&mut w)?,
            ServerPacket::GameData(data) => {
                w.u8(data.start);
                w.u8(data.tics.len() as u8);
                for tic in &data.tics {
                    w.s16(tic.latency);
                    let mut mask = 0u8;
                    for (index, _) in &tic.players {
                        if *index >= NET_MAXPLAYERS as u8 {
                            return Err(EncodeError::CountOutOfRange {
                                field: "gamedata.playeringame",
                                count: *index as usize,
                                max: NET_MAXPLAYERS,
                            });
                        }
                        mask |= 1 << index;
                    }
                    w.u8(mask);
                    for (_, diff) in &tic.players {
                        diff.encode(&mut w, lowres_turn);
                    }
                }
            }
            ServerPacket::ReliableAck { next_seq } => w.u8(*next_seq),
            ServerPacket::GameDataResend { start, count } => {
                w.u32(*start);
                w.u8(*count);
            }
            ServerPacket::ConsoleMessage { message } => {
                w.nul_string("console.message", message, None, true)?;
            }
            ServerPacket::QueryResponse(query) => {
                w.nul_string("query.version", &query.version, None, true)?;
                w.u8(query.server_state);
                w.u8(query.num_players);
                w.u8(query.max_players);
                w.u8(query.gamemode);
                w.u8(query.gamemission);
                w.nul_string("query.description", &query.description, None, true)?;
                if let Some(protocols) = &query.protocols {
                    if protocols.len() > 16 {
                        return Err(EncodeError::CountOutOfRange {
                            field: "query.protocols",
                            count: protocols.len(),
                            max: 16,
                        });
                    }
                    w.u8(protocols.len() as u8);
                    for protocol in protocols {
                        w.nul_string("query.protocol", protocol, None, true)?;
                    }
                }
            }
            ServerPacket::Launch { num_players } => w.u8(*num_players),
        }
        Ok(w.buf)
    }
}
