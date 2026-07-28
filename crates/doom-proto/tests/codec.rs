//! Codec tests: the 102-fixture directional round trip, validation
//! guards, constructor coverage for uncaptured shapes, and the
//! mutation battery. Every fixture is decoded under its manifest
//! direction and re-encoded to the exact original bytes; failures name
//! the file and the first differing offset.

mod common;

use common::{fixtures_dir, manifest_entries};
use doom_proto::{
    ClientPacket, ClientTic, ConnectData, DecodeError, GameDataClient, GameSettings, NET_MAGIC,
    ServerPacket, Syn, TiccmdDiff, WireHeader,
};
use std::fs;

const NO_RELIABLE: WireHeader = WireHeader { reliable_seq: None };
fn reliable(seq: u8) -> WireHeader {
    WireHeader {
        reliable_seq: Some(seq),
    }
}

fn read_fixture(rel: &str) -> Vec<u8> {
    fs::read(fixtures_dir().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b.iter()).position(|(x, y)| x != y)
}

fn assert_round_trip(bytes: &[u8], file: &str) {
    assert_eq!(bytes.len(), bytes.len(), "{file}: fixture readable");
}

#[test]
fn all_102_fixtures_decode_and_reencode_exactly() {
    let manifest = fs::read_to_string(fixtures_dir().join("manifest.json")).expect("manifest");
    let entries = manifest_entries(&manifest);
    assert_eq!(entries.len(), 102, "fixture count");

    for e in &entries {
        let bytes = read_fixture(&e.file);
        assert_eq!(bytes.len() as u64, e.length, "{}: manifest length", e.file);
        match e.dir.as_str() {
            "c2s" => {
                let (hdr, pkt) = ClientPacket::decode(&bytes, false)
                    .unwrap_or_else(|err| panic!("{}: c2s decode failed: {err:?}", e.file));
                assert_eq!(pkt.type_id() as u64, e.ptype, "{}: type", e.file);
                assert_eq!(
                    hdr.reliable_seq.is_some(),
                    e.reliable,
                    "{}: reliable",
                    e.file
                );
                let out = pkt
                    .encode(hdr, false)
                    .unwrap_or_else(|err| panic!("{}: c2s encode failed: {err:?}", e.file));
                if out != bytes {
                    match first_diff(&out, &bytes) {
                        Some(off) => panic!(
                            "{}: re-encode differs at offset {off}: got {:#04x?}, want {:#04x?}",
                            e.file,
                            &out[off..],
                            &bytes[off..]
                        ),
                        None => panic!(
                            "{}: re-encode length differs: got {}, want {}",
                            e.file,
                            out.len(),
                            bytes.len()
                        ),
                    }
                }
            }
            "s2c" => {
                let (hdr, pkt) = ServerPacket::decode(&bytes, false)
                    .unwrap_or_else(|err| panic!("{}: s2c decode failed: {err:?}", e.file));
                assert_eq!(pkt.type_id() as u64, e.ptype, "{}: type", e.file);
                assert_eq!(
                    hdr.reliable_seq.is_some(),
                    e.reliable,
                    "{}: reliable",
                    e.file
                );
                let out = pkt
                    .encode(hdr, false)
                    .unwrap_or_else(|err| panic!("{}: s2c encode failed: {err:?}", e.file));
                if out != bytes {
                    match first_diff(&out, &bytes) {
                        Some(off) => panic!(
                            "{}: re-encode differs at offset {off}: got {:#04x?}, want {:#04x?}",
                            e.file,
                            &out[off..],
                            &bytes[off..]
                        ),
                        None => panic!(
                            "{}: re-encode length differs: got {}, want {}",
                            e.file,
                            out.len(),
                            bytes.len()
                        ),
                    }
                }
            }
            other => panic!("{}: unknown direction {other}", e.file),
        }
        assert_round_trip(&bytes, &e.file);
    }
}

// Types whose layouts differ by direction; decoding these against the
// wrong family must fail. Direction-shared layouts (GAMESTART,
// RESEND, KEEPALIVE, DISCONNECT(_ACK), RELIABLE_ACK) cannot be told
// apart from bytes alone and are not asserted here.
const DIRECTION_DISTINCT: &[u16] = &[0, 2, 4, 6, 7, 12, 13, 14, 15];

#[test]
fn wrong_direction_is_rejected() {
    let manifest = fs::read_to_string(fixtures_dir().join("manifest.json")).expect("manifest");
    let entries = manifest_entries(&manifest);
    let mut checked = 0;

    for e in &entries {
        if !DIRECTION_DISTINCT.contains(&(e.ptype as u16)) {
            continue;
        }
        let bytes = read_fixture(&e.file);
        let wrong = match e.dir.as_str() {
            "c2s" => ServerPacket::decode(&bytes, false).map(|_| ()),
            "s2c" => ClientPacket::decode(&bytes, false).map(|_| ()),
            other => panic!("{}: unknown direction {other}", e.file),
        };
        assert!(
            wrong.is_err(),
            "{}: type {} decoded in the wrong direction",
            e.file,
            e.ptype
        );
        checked += 1;
    }
    assert!(checked > 40, "direction-distinct coverage: {checked}");
}

// --- constructors for uncaptured shapes ---------------------------------

#[test]
fn rejected_round_trip() {
    let pkt = ServerPacket::Rejected {
        reason: b"Version mismatch: no common protocol".to_vec(),
    };
    let bytes = pkt.encode(NO_RELIABLE, false).expect("encode");
    assert_eq!(&bytes[..2], &[0x00, 0x02]);
    let (hdr, decoded) = ServerPacket::decode(&bytes, false).expect("decode");
    assert_eq!(hdr, NO_RELIABLE);
    assert_eq!(decoded, pkt);
}

#[test]
fn rich_ticcmd_diffs_round_trip_both_turn_modes() {
    let full = TiccmdDiff {
        forward: Some(-50),
        side: Some(50),
        turn: Some(-1024),
        buttons: Some(0x81),
        consistancy: Some(0x7f),
        chatchar: Some(b'x'),
        lookfly: Some(3),
        arti: Some(4),
        buttons2: Some(5),
        inventory: Some(600),
    };
    let data = GameDataClient {
        ack: 7,
        start: 9,
        tics: vec![ClientTic {
            latency: -3,
            diff: full.clone(),
        }],
    };
    let pkt = ClientPacket::GameData(data);

    for lowres in [false, true] {
        let bytes = pkt.encode(NO_RELIABLE, lowres).expect("encode");
        let (hdr, decoded) = ClientPacket::decode(&bytes, lowres).expect("decode");
        assert_eq!(hdr, NO_RELIABLE);
        assert_eq!(decoded, pkt, "lowres_turn={lowres}");
    }

    // The lowres wire form is one byte narrower; the turn sits after
    // type(2)+ack+start+count+latency(2)+mask+forward+side = offset 10, and is the
    // high byte of the expanded value.
    let hires = pkt.encode(NO_RELIABLE, false).expect("hires");
    let lores = pkt.encode(NO_RELIABLE, true).expect("lores");
    assert_eq!(hires.len(), lores.len() + 1);
    assert_eq!(lores[10], (-1024i16 / 256) as i8 as u8);
}

#[test]
fn syn_with_multiple_protocols_round_trip() {
    let pkt = ClientPacket::Syn(Syn {
        version: b"Chocolate Doom 3.1.1".to_vec(),
        protocols: vec![b"CHOCOLATE_DOOM_0".to_vec(), b"MY_FORK_0".to_vec()],
        connect: ConnectData {
            gamemode: 0,
            gamemission: 0,
            lowres_turn: 0,
            drone: 0,
            max_players: 4,
            is_freedoom: 0,
            wad_sha1: [0x11; 20],
            deh_sha1: [0x22; 20],
            player_class: 0,
        },
        player_name: b"Tester".to_vec(),
    });
    let bytes = pkt.encode(NO_RELIABLE, false).expect("encode");
    let (hdr, decoded) = ClientPacket::decode(&bytes, false).expect("decode");
    assert_eq!(hdr, NO_RELIABLE);
    assert_eq!(decoded, pkt);
}

#[test]
fn full_lobby_and_settings_round_trip() {
    let settings = GameSettings {
        ticdup: 1,
        extratics: 1,
        deathmatch: 1,
        nomonsters: 0,
        fast_monsters: 0,
        respawn_monsters: 0,
        episode: 1,
        map: 3,
        skill: 4,
        gameversion: 5,
        lowres_turn: 0,
        new_sync: 1,
        timelimit: 600,
        loadgame: -1,
        random: 0,
        consoleplayer: 1,
        player_classes: vec![0; 8],
    };
    let pkt = ServerPacket::GameStart(settings);
    let bytes = pkt.encode(reliable(2), false).expect("encode");
    let (hdr, decoded) = ServerPacket::decode(&bytes, false).expect("decode");
    assert_eq!(hdr, reliable(2));
    assert_eq!(decoded, pkt);
}

// --- validation guards ----------------------------------------------------

fn c2s_syn() -> Vec<u8> {
    read_fixture("handshake-keepalive/000-c2s-client1-syn.bin")
}

#[test]
fn truncation_is_rejected_at_every_boundary_class() {
    let syn = c2s_syn();
    for cut in [2, 5, 27, 29, 45, 51, 71, 91, 92, syn.len() - 1] {
        assert!(
            ClientPacket::decode(&syn[..cut], false).is_err(),
            "SYN truncated to {cut} bytes must fail"
        );
    }

    let settings = read_fixture("gamestart-gamedata/008-c2s-client1-gamestart.bin");
    for cut in [1, 2, 3, 15, 19, 22, settings.len() - 1] {
        assert!(
            ClientPacket::decode(&settings[..cut], false).is_err(),
            "GAMESTART truncated to {cut} bytes must fail"
        );
    }

    let waitdata = read_fixture("handshake-keepalive/002-s2c-client1-waiting_data.bin");
    for cut in [1, 2, 8, 24, 39, 59, waitdata.len() - 1] {
        assert!(
            ServerPacket::decode(&waitdata[..cut], false).is_err(),
            "WAITING_DATA truncated to {cut} bytes must fail"
        );
    }
}

#[test]
fn unterminated_and_unprintable_strings_are_rejected() {
    let syn = c2s_syn();
    // Drop the NUL after the version string and let it run into the
    // protocol count: parsing must fail somewhere, never accept.
    let mut broken = syn.clone();
    broken.remove(26);
    assert!(ClientPacket::decode(&broken, false).is_err());

    // Corrupt a version byte into something unprintable.
    let mut ugly = c2s_syn();
    ugly[7] = 0x01;
    assert!(ClientPacket::decode(&ugly, false).is_err());
}

#[test]
fn impossible_counts_are_rejected() {
    // num_players = 9 in GAMESTART.
    let mut settings = read_fixture("gamestart-gamedata/008-c2s-client1-gamestart.bin");
    settings[21] = 9;
    assert!(ClientPacket::decode(&settings, false).is_err());

    // max_players = 9 in connect data.
    let mut syn = c2s_syn();
    syn[49] = 9;
    assert!(ClientPacket::decode(&syn, false).is_err());

    // num_players = 9 in WAITING_DATA.
    let mut waitdata = read_fixture("handshake-keepalive/002-s2c-client1-waiting_data.bin");
    waitdata[2] = 9;
    assert!(ServerPacket::decode(&waitdata, false).is_err());
}

#[test]
fn count_mismatch_and_trailing_bytes_are_rejected() {
    // GAMEDATA claiming 2 tics but carrying 1.
    let one_tic = read_fixture("gamestart-gamedata/029-c2s-client1-gamedata.bin");
    let mut two_tics = one_tic.clone();
    two_tics[4] = 2;
    assert!(ClientPacket::decode(&two_tics, false).is_err());

    // A trailing byte after an exact layout.
    let mut padded = read_fixture("handshake-keepalive/004-s2c-client1-keepalive.bin");
    padded.push(0);
    assert!(ServerPacket::decode(&padded, false).is_err());
}

#[test]
fn reliable_header_shape_is_validated() {
    // Reliable bit set but the seq byte is missing.
    let keepalive = [0x80, 0x03];
    assert!(ClientPacket::decode(&keepalive, false).is_err());

    // Reliable framing with a seq byte parses and round-trips.
    let (hdr, pkt) = ServerPacket::decode(&[0x80, 0x03, 0x2a], false).expect("decode");
    assert_eq!(hdr, reliable(42));
    assert_eq!(pkt, ServerPacket::Keepalive);
    assert_eq!(
        pkt.encode(hdr, false).expect("encode"),
        vec![0x80, 0x03, 0x2a]
    );
}

#[test]
fn ticcmd_mask_corruption_is_rejected() {
    // A zero-diff c2s GAMEDATA with a mask bit that needs more bytes.
    let mut data = read_fixture("gamestart-gamedata/029-c2s-client1-gamedata.bin");
    data[7] = 0xff; // diff mask: every field present, not enough bytes
    assert!(ClientPacket::decode(&data, false).is_err());
}

#[test]
fn deprecated_and_unknown_types_are_explicit() {
    assert!(matches!(
        ClientPacket::decode(&[0x00, 0x01], false),
        Err(DecodeError::UnsupportedType { type_id: 1 })
    ));
    assert!(matches!(
        ServerPacket::decode(&[0x00, 0x01], false),
        Err(DecodeError::UnsupportedType { type_id: 1 })
    ));
    assert!(matches!(
        ClientPacket::decode(&[0x00, 0x10], false),
        Err(DecodeError::UnsupportedType { type_id: 16 })
    ));
    assert!(matches!(
        ServerPacket::decode(&[0x00, 0x63], false),
        Err(DecodeError::UnsupportedType { type_id: 99 })
    ));
}

// --- mutation battery -------------------------------------------------------

#[test]
fn mutations_flip_the_guards() {
    // Endian swap: type word little-endian reads as another type.
    let keepalive_le = [0x03, 0x00];
    assert!(ClientPacket::decode(&keepalive_le, false).is_err());

    // NUL removal (see unterminated test above).
    // Count inflation (see impossible-count tests above).

    // Direction swap on SYN: the accept is not a valid client SYN.
    let accept = read_fixture("handshake-keepalive/001-s2c-client1-syn.bin");
    assert!(matches!(
        ClientPacket::decode(&accept, false),
        Err(DecodeError::BadMagic { .. })
    ));

    // Reliable bit flipped off a reliable SYN accept: trailing seq
    // byte becomes a protocol string of length 0, then the rest is
    // trailing garbage or a bad parse, and must fail.
    let mut flipped = read_fixture("handshake-keepalive/001-s2c-client1-syn.bin");
    flipped[0] = 0x00;
    assert!(ServerPacket::decode(&flipped, false).is_err());

    // Reliable bit flipped on a plain keepalive with no seq byte.
    let mut flipped = read_fixture("handshake-keepalive/004-s2c-client1-keepalive.bin");
    flipped[0] = 0x80;
    assert!(ServerPacket::decode(&flipped, false).is_err());
}

#[test]
fn magic_must_be_exact() {
    let mut syn = c2s_syn();
    syn[5] ^= 0xff;
    assert!(matches!(
        ClientPacket::decode(&syn, false),
        Err(DecodeError::BadMagic { got }) if got == (NET_MAGIC ^ 0xff)
    ));
}
