//! Loading content: console detection, BIOS lookup, and emulator construction.

use crate::detect::{self, Console};
use crate::env;
use crate::frontend::SaveFiles;
use crate::options::Options;
use crate::session::{CoreSession, Session};
use crate::systems;
use crate::vfs;
use cdrom::CdRomError;
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
        vfs::read(&path).map_err(|err| {
            format!(
                "Missing firmware '{file_name}' in system directory ('{}'): {err}",
                path.display()
            )
        })
    }
}

/// The content being loaded, as described by the frontend.
#[derive(Debug, PartialEq, Eq)]
struct Content {
    /// Content file on disk; absent when it is inside an archive that was not extracted
    path: Option<PathBuf>,
    /// Directory containing the content file, or the archive it is inside
    dir: PathBuf,
    /// Content name without extension; the archive's name for archived content
    name: String,
    /// Lowercase extension of the content file, without the dot
    extension: String,
}

impl Content {
    /// From `GET_GAME_INFO_EXT`, which handles archives and extensions on the frontend's side.
    fn from_game_info_ext(info: env::GameInfoExt) -> Option<Self> {
        Some(Self {
            path: info.full_path.map(PathBuf::from),
            dir: info.dir.map(PathBuf::from).unwrap_or_default(),
            name: info.name?,
            extension: info.ext?.to_ascii_lowercase(),
        })
    }

    /// Fallback for frontends without `GET_GAME_INFO_EXT`: parse `retro_game_info::path`, which
    /// may be in `RetroArch`'s `archive.zip#file.ext` form.
    fn from_path(path: &str) -> Result<Self, String> {
        let (outer, inner) = split_archive_path(path);
        let file_name = |path: &'_ str| path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned();
        let split_extension = |file_name: &str| {
            file_name
                .rsplit_once('.')
                .map(|(stem, ext)| (stem.to_owned(), ext.to_ascii_lowercase()))
        };

        let content_file_name = file_name(inner.unwrap_or(outer));
        let (stem, extension) = split_extension(&content_file_name)
            .ok_or_else(|| format!("Unknown file type: '{path}'"))?;
        // Match RetroArch's GET_GAME_INFO_EXT, which names archived content after the archive
        let name = match inner {
            Some(_) => {
                let archive_file_name = file_name(outer);
                split_extension(&archive_file_name).map_or(archive_file_name, |(stem, _)| stem)
            }
            None => stem,
        };

        let outer = Path::new(outer);
        Ok(Self {
            path: inner.is_none().then(|| outer.to_path_buf()),
            dir: outer.parent().map(Path::to_path_buf).unwrap_or_default(),
            name,
            extension,
        })
    }

    fn require_path(&self) -> Result<&Path, String> {
        self.path.as_deref().ok_or_else(|| {
            format!("'{}.{}' cannot be loaded from inside an archive", self.name, self.extension)
        })
    }
}

/// Split `RetroArch`'s `archive.zip#file.ext` path form into the archive and the file inside it.
fn split_archive_path(path: &str) -> (&str, Option<&str>) {
    let lowercase = path.to_ascii_lowercase();
    [".zip#", ".7z#", ".apk#"]
        .iter()
        .find_map(|delim| lowercase.find(delim).map(|i| i + delim.len() - 1))
        .map_or((path, None), |i| (&path[..i], Some(&path[i + 1..])))
}

/// Describe the first (or only) content file being loaded.
fn resolve_content(path: Option<&str>) -> Result<Content, String> {
    match env::game_info_ext().and_then(Content::from_game_info_ext) {
        Some(content) => Ok(content),
        None => Content::from_path(path.ok_or("Content path is required")?),
    }
}

/// Firmware and save locations, falling back to `content_dir` if the frontend doesn't provide them.
fn dirs_and_saves(content_dir: &Path, save_name: &str) -> (Dirs, SaveFiles) {
    let save_dir = env::save_directory().unwrap_or_else(|| content_dir.to_path_buf());
    let system_dir = env::system_directory().unwrap_or_else(|| content_dir.to_path_buf());
    (Dirs { system: system_dir }, SaveFiles::new(save_dir, save_base_name(save_name)))
}

fn read_rom(content: &Content, data: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let rom = match data {
        Some(data) => data.to_vec(),
        None => {
            let path = content.require_path()?;
            vfs::read(path).map_err(|err| format!("Unable to read '{}': {err}", path.display()))?
        }
    };

    // Otherwise a lone disc track (e.g. from a zipped CUE/BIN) would run as a garbage cartridge
    if content.extension == "bin" && detect::is_disc_data_track(&rom) {
        return Err("This .bin file is a CD image track, not a cartridge; load the disc's .cue, \
                    .chd, or .m3u instead (zipped CUE/BIN images are not supported)"
            .into());
    }

    Ok(rom)
}

pub fn load_game(path: Option<&str>, data: Option<&[u8]>) -> Result<LoadedGame, String> {
    let content = resolve_content(path)?;
    let extension = content.extension.as_str();
    let (dirs, saves) = dirs_and_saves(&content.dir, &content.name);
    let options = Options::read();

    if extension == "m3u" {
        let discs = parse_m3u(content.require_path()?)?;
        let first = discs.first().ok_or("M3U playlist is empty")?.clone();
        let mut game = load_disc(&first, &dirs, saves, &options)?;
        game.discs = discs;
        return Ok(game);
    }

    if detect::DISC_EXTENSIONS.contains(&extension) {
        return load_disc(content.require_path()?, &dirs, saves, &options);
    }

    let rom = read_rom(&content, data)?;
    let console = detect::detect_cartridge(extension, &rom)
        .ok_or_else(|| format!("Unsupported file extension '.{extension}'"))?;
    log::info!("Detected console {console:?} for '{}.{extension}'", content.name);

    let session = load_cartridge(console, rom, &dirs, saves, &options)?;
    Ok(LoadedGame { session, console, discs: vec![] })
}

/// Boot the Sega CD BIOS with an empty disc drive, for running without content. A disc can be
/// inserted later through the disk control interface.
pub fn load_no_game() -> Result<LoadedGame, String> {
    let options = Options::read();
    let region = systems::genesis_region(&options).unwrap_or(GenesisRegion::Americas);
    let bios_name = sega_cd_bios_name(region);

    // Standalone jgenesis names BIOS-only saves (backup RAM) after the BIOS file
    let save_name = bios_name.rsplit_once('.').map_or(bios_name, |(stem, _)| stem);
    let (dirs, saves) = dirs_and_saves(Path::new(""), save_name);
    let bios = dirs.read_firmware(bios_name)?;
    log::info!("No content; booting Sega CD BIOS '{bios_name}'");

    let session =
        genesis_session(GenesisHardware::SegaCd, None, Some(bios), None, saves, &options)?;
    Ok(LoadedGame { session, console: Console::SegaCd, discs: vec![] })
}

/// Sega CD "Mode 1": boot a Genesis or 32X cartridge with the Sega CD attached and a disc
/// inserted, e.g. for MSU-MD games that play CD audio.
pub fn load_sega_cd_mode_1(
    cartridge_path: Option<&str>,
    cartridge_data: Option<&[u8]>,
    disc_path: Option<&str>,
) -> Result<LoadedGame, String> {
    let content = resolve_content(cartridge_path)?;
    let disc_path = PathBuf::from(disc_path.ok_or("Sega CD Mode 1 requires a disc image")?);
    let (dirs, saves) = dirs_and_saves(&content.dir, &content.name);
    let options = Options::read();

    let rom = read_rom(&content, cartridge_data)?;
    let (console, hardware) = match detect::detect_cartridge(&content.extension, &rom) {
        Some(Console::Genesis | Console::SegaCd) => (Console::SegaCd, GenesisHardware::SegaCd),
        Some(Console::Sega32X | Console::SegaCd32X) => {
            (Console::SegaCd32X, GenesisHardware::SegaCd32X)
        }
        _ => {
            return Err(format!(
                "'{}.{}' is not a Genesis or 32X cartridge",
                content.name, content.extension
            ));
        }
    };
    log::info!("Sega CD Mode 1 with {hardware}: '{}' + '{}'", content.name, disc_path.display());

    let region = systems::genesis_region(&options).unwrap_or(GenesisRegion::Americas);
    let bios = dirs.read_firmware(sega_cd_bios_name(region))?;
    let disc = open_disc(&disc_path)?;

    let session = genesis_session(hardware, Some(rom), Some(bios), Some(disc), saves, &options)?;
    Ok(LoadedGame { session, console, discs: vec![disc_path] })
}

/// Save files are named after the content, with any " (Disc N)" removed so that every disc of a
/// multi-disc game shares one save file (same as standalone jgenesis).
fn save_base_name(content_name: &str) -> String {
    let mut name = content_name.to_string();
    if let Some(start) = name.find(" (Disc ")
        && let Some(len) = name[start..].find(')')
    {
        name.replace_range(start..=start + len, "");
    }
    name
}

fn parse_m3u(path: &Path) -> Result<Vec<PathBuf>, String> {
    let contents = vfs::read_to_string(path)
        .map_err(|err| format!("Unable to read M3U playlist '{}': {err}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new(""));

    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| base.join(line))
        .collect())
}

/// CHD images go through the frontend's VFS when it has one, which means reading the whole image
/// into memory: upstream `cdrom` can only stream from real files. CUE/BIN always streams from the
/// filesystem, since `cdrom` reads the BIN files named in the CUE sheet itself.
pub fn open_disc(path: &Path) -> Result<CdRom, String> {
    let format = CdRomFileFormat::from_file_path(path)
        .ok_or_else(|| format!("Unsupported disc image format: '{}'", path.display()))?;
    let open_err = |err: &dyn std::fmt::Display| {
        format!("Unable to open disc image '{}': {err}", path.display())
    };

    if format == CdRomFileFormat::Chd && vfs::is_available() {
        let size = vfs::file_size(path).map_err(|err| open_err(&err))?;
        check_free_memory(path, size)?;
        let chd = vfs::read(path).map_err(|err| open_err(&err))?;
        return CdRom::open_chd_in_memory(chd).map_err(|err| open_err(&err));
    }
    CdRom::open(path, format).map_err(|err| match err {
        // The file exists, but only the frontend can reach it (e.g. smb:// or nfs:// paths, or UWP
        // storage). Upstream cdrom reads CUE/BIN with std::fs, and frontends skip copying content
        // to a local file for cores that use the VFS. See issue #12.
        CdRomError::CueOpen { path: ref file, .. }
        | CdRomError::FsMetadata { path: ref file, .. }
        | CdRomError::BinOpen { path: ref file, .. }
            if vfs::is_available() && vfs::exists(Path::new(file)) =>
        {
            format!(
                "CUE/BIN disc images can't be loaded from this location yet ('{file}'); convert the \
                 disc to CHD, or copy it to a local folder"
            )
        }
        err => open_err(&err),
    })
}

/// Refuse to read a disc image into memory when the frontend reports too little free memory,
/// rather than risk the OS killing the frontend (e.g. on low-memory Android devices).
fn check_free_memory(path: &Path, size: u64) -> Result<(), String> {
    const MIB: u64 = 1024 * 1024;

    let Some(free) = env::free_memory() else { return Ok(()) };
    // Free memory is a snapshot, so leave headroom for everything else
    let budget = free / 4 * 3;
    if size <= budget {
        return Ok(());
    }

    Err(format!(
        "Not enough free memory to load disc image '{}' ({} MiB, {} MiB free)",
        path.display(),
        size.div_ceil(MIB),
        free / MIB
    ))
}

fn load_disc(
    path: &Path,
    dirs: &Dirs,
    saves: SaveFiles,
    options: &Options,
) -> Result<LoadedGame, String> {
    let mut disc = open_disc(path)?;
    let console = detect::detect_disc(&mut disc);
    log::info!("Detected console {console:?} for '{}'", path.display());

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
        Some(Box::new(move || vfs::read(&path).map_err(|err| (err, path.display().to_string()))))
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

    fn content(path: Option<&str>, dir: &str, name: &str, extension: &str) -> Content {
        Content {
            path: path.map(PathBuf::from),
            dir: PathBuf::from(dir),
            name: name.into(),
            extension: extension.into(),
        }
    }

    #[test]
    fn save_names_strip_disc_numbers() {
        assert_eq!(save_base_name("Sonic (USA)"), "Sonic (USA)");
        assert_eq!(save_base_name("Lunar (USA) (Disc 2)"), "Lunar (USA)");
    }

    #[test]
    fn content_from_path() {
        assert_eq!(
            Content::from_path("/roms/Sonic.MD"),
            Ok(content(Some("/roms/Sonic.MD"), "/roms", "Sonic", "md"))
        );
        // The directory depends on the host's path separators, so only check the file name parts
        let windows = Content::from_path(r"C:\roms\game.sfc").unwrap();
        assert_eq!((windows.name.as_str(), windows.extension.as_str()), ("game", "sfc"));
        assert_eq!(
            Content::from_path("/roms/Puyo Puyo #2.md"),
            Ok(content(Some("/roms/Puyo Puyo #2.md"), "/roms", "Puyo Puyo #2", "md"))
        );
        assert!(Content::from_path("/roms/no_extension").is_err());
    }

    #[test]
    fn content_from_archive_path() {
        assert_eq!(
            Content::from_path("/roms/pack.zip#Sonic (USA).gen"),
            Ok(content(None, "/roms", "pack", "gen"))
        );
        assert_eq!(
            Content::from_path("/roms/Pack.7Z#sub/Game.SFC"),
            Ok(content(None, "/roms", "Pack", "sfc"))
        );
    }

    #[test]
    fn content_from_game_info_ext() {
        let info = env::GameInfoExt {
            full_path: None,
            dir: Some("/roms".into()),
            name: Some("pack".into()),
            ext: Some("gen".into()),
        };
        assert_eq!(Content::from_game_info_ext(info), Some(content(None, "/roms", "pack", "gen")));

        let info =
            env::GameInfoExt { full_path: None, dir: None, name: None, ext: Some("md".into()) };
        assert_eq!(Content::from_game_info_ext(info), None);
    }
}
