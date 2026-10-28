//! Console detection from file extension and contents.
//!
//! Ported from `frontend/jgenesis-native-driver/src/extensions.rs` in jgenesis, adapted to work on
//! in-memory ROM data.

use cdrom::cdtime::CdTime;
use cdrom::cue::TrackType;
use cdrom::reader::CdRom;

/// Extensions loaded from a path on disk.
pub const DISC_EXTENSIONS: &[&str] = &["cue", "chd", "m3u"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    Sg1000,
    MasterSystem,
    GameGear,
    Genesis,
    SegaCd,
    Sega32X,
    SegaCd32X,
    Nes,
    Snes,
    GameBoy,
    GameBoyAdvance,
    PcEngine,
    PcEngineCd,
}

/// Detect the console for a cartridge ROM from its extension and header.
#[must_use]
pub fn detect_cartridge(extension: &str, rom: &[u8]) -> Option<Console> {
    let console = match extension {
        "sg" => Console::Sg1000,
        "sms" => Console::MasterSystem,
        "gg" => Console::GameGear,
        "nes" => Console::Nes,
        "sfc" | "smc" => Console::Snes,
        "gb" | "gbc" => Console::GameBoy,
        "gba" => Console::GameBoyAdvance,
        "pce" => Console::PcEngine,
        "bin" if is_gba_rom(rom) => Console::GameBoyAdvance,
        "gen" | "md" | "bin" | "smd" | "32x" => guess_genesis_console(rom),
        _ => return None,
    };
    Some(console)
}

/// Whether a `.bin` file is a CD image's data track rather than a cartridge ROM, e.g. track 1 of a
/// zipped CUE/BIN image that the frontend extracted on its own.
#[must_use]
pub fn is_disc_data_track(rom: &[u8]) -> bool {
    // Raw 2352-byte sectors start with this sync pattern, which can't begin a Genesis ROM (its
    // reset vector would be $FFFFFFFF)
    const SECTOR_SYNC: [u8; 12] =
        [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

    // 2048-byte sectors (ISO) have no sync pattern, but Sega CD discs start with an ID string
    rom.starts_with(&SECTOR_SYNC)
        || [b"SEGADISCSYSTEM" as &[u8], b"SEGADATADISC"].iter().any(|id| rom.starts_with(id))
}

// GBA cartridge headers contain the fixed value 0x96 at 0xB2 and the Nintendo logo at 0x04,
// which begins with these bytes
fn is_gba_rom(rom: &[u8]) -> bool {
    const LOGO_START: [u8; 4] = [0x24, 0xFF, 0xAE, 0x51];
    rom.len() >= 0xC0 && rom[0xB2] == 0x96 && rom[0x04..0x08] == LOGO_START
}

fn guess_genesis_console(header: &[u8]) -> Console {
    let start = s32x_core::SECURITY_PROGRAM_CARTRIDGE_ADDR;
    let end = start + s32x_core::SECURITY_PROGRAM_LEN;
    let contains_s32x_security_program =
        header.len() >= end && &header[start..end] == s32x_core::security_program();

    // 'C' in the devices section indicates Sega CD support: https://plutiedev.com/rom-header#devices
    // Exclude J-Cart games (have device string 'OJKRPTBVFCA')
    let mut supports_sega_cd = header.len() >= 0x1A0
        && header[0x190..0x1A0].contains(&b'C')
        && &header[0x190..0x19B] != b"OJKRPTBVFCA";

    // Special case Flux (audio CD visualizer), header doesn't indicate Sega CD support
    supports_sega_cd |= header.len() >= 0x18B && &header[0x180..0x18B] == b"GM T-70416-";

    if supports_sega_cd && contains_s32x_security_program {
        Console::SegaCd32X
    } else if contains_s32x_security_program {
        Console::Sega32X
    } else if supports_sega_cd {
        Console::SegaCd
    } else {
        Console::Genesis
    }
}

/// Detect whether a CD-ROM image is a Sega CD, Sega CD 32X, or PC Engine CD game.
#[must_use]
pub fn detect_disc(disc: &mut CdRom) -> Console {
    const DEFAULT: Console = Console::SegaCd;

    fn sega_cd_with_32x_check(sector_buffer: &[u8; cdrom::BYTES_PER_SECTOR as usize]) -> Console {
        if segacd_core::is_cd_32x_first_sector(sector_buffer) {
            Console::SegaCd32X
        } else {
            Console::SegaCd
        }
    }

    let Some(first_data_track) =
        disc.cue().tracks_iter().find(|track| track.track_type == TrackType::Data)
    else {
        return DEFAULT;
    };

    // Every Sega CD game has track 1 as a data track (BIOS wouldn't boot it otherwise)
    // PC Engine games usually have track 1 as audio and track 2 as data, but not always
    if first_data_track.number != 1 {
        return Console::PcEngineCd;
    }

    // Every Sega CD game disc should have the string "SEGADISCSYSTEM" or "SEGADATADISC" at the
    // very beginning of the first data sector
    let mut first_sector = [0; cdrom::BYTES_PER_SECTOR as usize];
    if disc.read_sector(1, CdTime::SECTOR_0_START, &mut first_sector).is_err() {
        return DEFAULT;
    }

    for s in [b"SEGADISCSYSTEM" as &[u8], b"SEGADATADISC" as &[u8]] {
        if &first_sector[16..16 + s.len()] == s {
            return sega_cd_with_32x_check(&first_sector);
        }
    }

    // Every PC Engine game should have the ID string "PC Engine CD-ROM SYSTEM" at the beginning of
    // the second data sector
    let mut second_sector = [0; cdrom::BYTES_PER_SECTOR as usize];
    if disc
        .read_sector(1, CdTime::SECTOR_0_START + CdTime::new(0, 0, 1), &mut second_sector)
        .is_err()
    {
        return sega_cd_with_32x_check(&first_sector);
    }

    // First 32 bytes of the second sector are IPL data, ID should be right after that
    let pce_id = b"PC Engine CD-ROM SYSTEM";
    if &second_sector[16 + 32..16 + 32 + pce_id.len()] == pce_id {
        return Console::PcEngineCd;
    }

    log::warn!("Unable to reliably guess console for CD-ROM image; defaulting to Sega CD");

    sega_cd_with_32x_check(&first_sector)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_genesis_header() {
        let mut rom = vec![0; 0x400];
        rom[0x190..0x193].copy_from_slice(b"J  ");
        assert_eq!(detect_cartridge("md", &rom), Some(Console::Genesis));
        assert_eq!(detect_cartridge("bin", &rom), Some(Console::Genesis));
    }

    #[test]
    fn disc_data_tracks_are_not_cartridges() {
        let mut raw = vec![0xFF; 2352];
        raw[0] = 0x00;
        raw[11] = 0x00;
        raw[0x10..0x1E].copy_from_slice(b"SEGADISCSYSTEM");
        assert!(is_disc_data_track(&raw));

        let iso = b"SEGADISCSYSTEM  ".repeat(128);
        assert!(is_disc_data_track(&iso));

        // Genesis ROMs start with the 68000 stack pointer and reset vector
        let mut rom = vec![0; 0x400];
        rom[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFE, 0x00, 0x00, 0x02, 0x00]);
        assert!(!is_disc_data_track(&rom));
    }

    #[test]
    fn gba_bin_detected_by_header() {
        let mut rom = vec![0; 0x200];
        rom[0x04..0x08].copy_from_slice(&[0x24, 0xFF, 0xAE, 0x51]);
        rom[0xB2] = 0x96;
        assert_eq!(detect_cartridge("bin", &rom), Some(Console::GameBoyAdvance));
    }
}
