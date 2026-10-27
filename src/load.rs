//! Loading content: console detection, BIOS lookup, and emulator construction.

use crate::detect::{self, Console};
use crate::env;
use crate::frontend::SaveFiles;
use crate::options::Options;
use crate::session::{CoreSession, Session};
use crate::systems;
use cdrom::reader::{CdRom, CdRomFileFormat};
use gb_config::GameBoyInputs;
use gb_core::api::{BootRoms, GameBoyEmulator};
use gba_config::GbaInputs;
use gba_core::api::GameBoyAdvanceEmulator;
use genesis_config::{GenesisEmulatorConfig, GenesisRegion};
use genesis_core::GenesisEmulator;
use genesis_core::api::GenesisHardware;
use nes_core::api::NesEmulator;
use nes_core::input::NesInputs;
use pce_config::PceInputs;
use pce_core::api::PcEngineEmulator;
use smsgg_config::SmsGgInputs;
use smsgg_core::{SmsGgEmulator, SmsGgHardware};
use snes_config::SnesJoypadState;
use snes_core::api::{CoprocessorRomFn, CoprocessorRoms, SnesEmulator, SnesEmulatorConfig};
use snes_core::input::{SnesController, SnesInputs};
use std::fs;
use std::path::{Path, PathBuf};

const SEGA_CD_BIOS_US: &str = "bios_CD_U.bin";
const SEGA_CD_BIOS_EU: &str = "bios_CD_E.bin";
const SEGA_CD_BIOS_JP: &str = "bios_CD_J.bin";
const GBA_BIOS: &str = "gba_bios.bin";
const PCE_SYSTEM_CARD: &str = "syscard3.pce";

pub struct LoadedGame {
    pub session: Box<dyn CoreSession>,
    pub console: Console,
    /// Disc images available for disc swapping (from an M3U playlist, or the single loaded disc)
    pub discs: Vec<PathBuf>,
}

struct Dirs {
    system: PathBuf,
}

impl Dirs {
    fn read_firmware(&self, file_name: &str) -> Result<Vec<u8>, String> {
        let path = self.system.join(file_name);
        fs::read(&path).map_err(|err| {
            format!(
                "Missing firmware '{file_name}' in system directory ('{}'): {err}",
                path.display()
            )
        })
    }
}

pub fn load_game(path: Option<&str>, data: Option<&[u8]>) -> Result<LoadedGame, String> {
    let path_str = path.ok_or("Content path is required")?;
    let extension =
        detect::extension(path_str).ok_or_else(|| format!("Unknown file type: '{path_str}'"))?;
    let path = Path::new(path_str);

    let content_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let save_dir = env::save_directory().unwrap_or_else(|| content_dir.clone());
    let system_dir = env::system_directory().unwrap_or_else(|| content_dir.clone());
    let dirs = Dirs { system: system_dir };

    let saves = SaveFiles::new(save_dir, save_base_name(path_str));
    let options = Options::read();

    if extension == "m3u" {
        let discs = parse_m3u(path)?;
        let first = discs.first().ok_or("M3U playlist is empty")?.clone();
        let mut game = load_disc(&first, &dirs, saves, &options)?;
        game.discs = discs;
        return Ok(game);
    }

    if detect::DISC_EXTENSIONS.contains(&extension.as_str()) {
        return load_disc(path, &dirs, saves, &options);
    }

    let rom = match data {
        Some(data) => data.to_vec(),
        None => fs::read(path).map_err(|err| format!("Unable to read '{path_str}': {err}"))?,
    };
    let console = detect::detect_cartridge(&extension, &rom)
        .ok_or_else(|| format!("Unsupported file extension '.{extension}'"))?;
    log::info!("Detected console {console:?} for '{path_str}'");

    let session = load_cartridge(console, rom, &dirs, saves, &options)?;
    Ok(LoadedGame { session, console, discs: vec![] })
}

/// Save files are named after the content file, with any " (Disc N)" removed so that every disc
/// of a multi-disc game shares one save file (same as standalone jgenesis).
fn save_base_name(path: &str) -> String {
    let file_name = path.rsplit(['/', '\\', '#']).next().unwrap_or(path);
    let stem = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem);

    let mut name = stem.to_string();
    if let Some(start) = name.find(" (Disc ")
        && let Some(len) = name[start..].find(')')
    {
        name.replace_range(start..=start + len, "");
    }
    name
}

fn parse_m3u(path: &Path) -> Result<Vec<PathBuf>, String> {
    let contents = fs::read_to_string(path)
        .map_err(|err| format!("Unable to read M3U playlist '{}': {err}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new(""));

    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| base.join(line))
        .collect())
}

pub fn open_disc(path: &Path) -> Result<CdRom, String> {
    let format = CdRomFileFormat::from_file_path(path)
        .ok_or_else(|| format!("Unsupported disc image format: '{}'", path.display()))?;
    CdRom::open(path, format)
        .map_err(|err| format!("Unable to open disc image '{}': {err}", path.display()))
}

fn load_disc(
    path: &Path,
    dirs: &Dirs,
    saves: SaveFiles,
    options: &Options,
) -> Result<LoadedGame, String> {
    let console = detect::detect_disc(path);
    log::info!("Detected console {console:?} for '{}'", path.display());

    let mut disc = open_disc(path)?;

    let session: Box<dyn CoreSession> = if console == Console::PcEngineCd {
        let system_card = dirs.read_firmware(PCE_SYSTEM_CARD)?;
        Box::new(Session::<PcEngineEmulator>::new(
            systems::pce_config(),
            PceInputs::default(),
            options,
            saves,
            |config, saves| Ok(PcEngineEmulator::create(system_card, Some(disc), config, saves)),
        )?)
    } else {
        let hardware = if console == Console::SegaCd32X {
            GenesisHardware::SegaCd32X
        } else {
            GenesisHardware::SegaCd
        };

        let region = systems::genesis_region(options)
            .or_else(|| segacd_core::parse_disc_region(&mut disc).ok())
            .unwrap_or(GenesisRegion::Americas);
        let bios = dirs.read_firmware(sega_cd_bios_name(region))?;

        genesis_session(hardware, None, Some(bios), Some(disc), saves, options)?
    };

    Ok(LoadedGame { session, console, discs: vec![path.to_path_buf()] })
}

fn sega_cd_bios_name(region: GenesisRegion) -> &'static str {
    match region {
        GenesisRegion::Americas => SEGA_CD_BIOS_US,
        GenesisRegion::Europe => SEGA_CD_BIOS_EU,
        GenesisRegion::Japan => SEGA_CD_BIOS_JP,
    }
}

fn genesis_session(
    hardware: GenesisHardware,
    rom: Option<Vec<u8>>,
    bios: Option<Vec<u8>>,
    disc: Option<CdRom>,
    saves: SaveFiles,
    options: &Options,
) -> Result<Box<dyn CoreSession>, String> {
    Ok(Box::new(Session::<GenesisEmulator>::new(
        GenesisEmulatorConfig::default(),
        systems::genesis_inputs(),
        options,
        saves,
        |config, saves| {
            GenesisEmulator::create(hardware, rom, bios, disc, config, saves)
                .map_err(|err| err.to_string())
        },
    )?))
}

fn load_cartridge(
    console: Console,
    rom: Vec<u8>,
    dirs: &Dirs,
    saves: SaveFiles,
    options: &Options,
) -> Result<Box<dyn CoreSession>, String> {
    let session: Box<dyn CoreSession> = match console {
        Console::Genesis => {
            genesis_session(GenesisHardware::Standalone, Some(rom), None, None, saves, options)?
        }
        Console::Sega32X => {
            genesis_session(GenesisHardware::Sega32X, Some(rom), None, None, saves, options)?
        }
        Console::SegaCd | Console::SegaCd32X => {
            // Cartridge that supports Sega CD hardware; boot with Sega CD attached if the BIOS is
            // available, otherwise fall back to running it as a plain cartridge
            let region = systems::genesis_region(options).unwrap_or(GenesisRegion::Americas);
            let (with_cd, without_cd) = if console == Console::SegaCd32X {
                (GenesisHardware::SegaCd32X, GenesisHardware::Sega32X)
            } else {
                (GenesisHardware::SegaCd, GenesisHardware::Standalone)
            };
            match dirs.read_firmware(sega_cd_bios_name(region)) {
                Ok(bios) => genesis_session(with_cd, Some(rom), Some(bios), None, saves, options)?,
                Err(err) => {
                    log::warn!("{err}; running without Sega CD hardware");
                    genesis_session(without_cd, Some(rom), None, None, saves, options)?
                }
            }
        }
        Console::Sg1000 | Console::MasterSystem | Console::GameGear => {
            let hardware = match console {
                Console::Sg1000 => SmsGgHardware::Sg1000,
                Console::GameGear => SmsGgHardware::GameGear,
                _ => SmsGgHardware::MasterSystem,
            };
            Box::new(Session::<SmsGgEmulator>::new(
                systems::smsgg_config(),
                SmsGgInputs::default(),
                options,
                saves,
                |config, saves| Ok(SmsGgEmulator::create(Some(rom), None, hardware, config, saves)),
            )?)
        }
        Console::Nes => Box::new(Session::<NesEmulator>::new(
            systems::nes_config(),
            NesInputs::default(),
            options,
            saves,
            |config, saves| NesEmulator::create(rom, config, saves).map_err(|err| err.to_string()),
        )?),
        Console::Snes => {
            let coprocessor_roms = snes_coprocessor_roms(&dirs.system);
            Box::new(Session::<SnesEmulator>::new(
                SnesEmulatorConfig::default(),
                SnesInputs {
                    p1: SnesController::Gamepad(SnesJoypadState::default()),
                    p2: SnesController::Gamepad(SnesJoypadState::default()),
                },
                options,
                saves,
                |config, saves| {
                    SnesEmulator::create(rom, config, coprocessor_roms, saves)
                        .map_err(|err| err.to_string())
                },
            )?)
        }
        Console::GameBoy => Box::new(Session::<GameBoyEmulator>::new(
            systems::gb_config(),
            GameBoyInputs::default(),
            options,
            saves,
            |config, saves| {
                GameBoyEmulator::create(rom, BootRoms { dmg: None, cgb: None }, config, saves)
                    .map_err(|err| err.to_string())
            },
        )?),
        Console::GameBoyAdvance => {
            let bios = dirs.read_firmware(GBA_BIOS)?;
            Box::new(Session::<GameBoyAdvanceEmulator>::new(
                systems::gba_config(),
                GbaInputs::default(),
                options,
                saves,
                |config, saves| {
                    GameBoyAdvanceEmulator::create(rom, bios, config, saves)
                        .map_err(|err| err.to_string())
                },
            )?)
        }
        Console::PcEngine => Box::new(Session::<PcEngineEmulator>::new(
            systems::pce_config(),
            PceInputs::default(),
            options,
            saves,
            |config, saves| Ok(PcEngineEmulator::create(rom, None, config, saves)),
        )?),
        Console::PcEngineCd => return Err("PC Engine CD games must be loaded as CUE or CHD".into()),
    };

    Ok(session)
}

/// SNES coprocessor firmware, in the combined program+data format used by snes9x and jgenesis
/// (e.g. `dsp1.rom`). Loaded lazily, only if the cartridge needs it.
fn snes_coprocessor_roms(system_dir: &Path) -> CoprocessorRoms {
    let rom_fn = |file_name: &str| -> Option<Box<CoprocessorRomFn>> {
        let path = system_dir.join(file_name);
        Some(Box::new(move || fs::read(&path).map_err(|err| (err, path.display().to_string()))))
    };

    CoprocessorRoms {
        dsp1: rom_fn("dsp1.rom"),
        dsp2: rom_fn("dsp2.rom"),
        dsp3: rom_fn("dsp3.rom"),
        dsp4: rom_fn("dsp4.rom"),
        st010: rom_fn("st010.rom"),
        st011: rom_fn("st011.rom"),
        st018: rom_fn("st018.rom"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_names_strip_disc_numbers() {
        assert_eq!(save_base_name("/roms/Sonic (USA).md"), "Sonic (USA)");
        assert_eq!(save_base_name("/cd/Lunar (USA) (Disc 2).cue"), "Lunar (USA)");
        assert_eq!(save_base_name("/roms/pack.zip#Game.sfc"), "Game");
    }
}
