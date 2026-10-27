//! [`System`] implementations for each jgenesis emulator: button mappings, default configs, and
//! core option handling.
//!
//! Default config values match the defaults of the standalone jgenesis frontend.

use crate::ffi::{self, retro_device_subclass};
use crate::options::Options;
use crate::session::System;
use cdrom::reader::CdRom;
use gb_config::{GameBoyButton, GbAspectRatio, GbAudioResampler, GbPalette};
use gb_core::api::{GameBoyEmulator, GameBoyEmulatorConfig};
use gba_config::{GbaAspectRatio, GbaButton};
use gba_core::api::{GameBoyAdvanceEmulator, GbaEmulatorConfig};
use genesis_config::{
    GenesisAspectRatio, GenesisButton, GenesisController, GenesisControllerType,
    GenesisEmulatorConfig, GenesisInputs, GenesisRegion, PcmInterpolation, S32XVideoOut,
};
use genesis_core::GenesisEmulator;
use jgenesis_common::frontend::{ColorCorrection, FiniteF32, TimingMode};
use nes_config::{NesAspectRatio, NesAudioResampler, NesButton, NesPalette, Overscan};
use nes_core::api::{NesEmulator, NesEmulatorConfig};
use pce_config::{
    PceAspectRatio, PceButton, PceGamepadType, PceInputDevice, PcePaletteType, PcePsgResampler,
    PceRegion, PceSystemCardModel,
};
use pce_core::api::{PcEngineEmulator, PceEmulatorConfig};
use smsgg_config::{GgAspectRatio, SmsAspectRatio, SmsGgButton, SmsGgRegion, SmsModel};
use smsgg_core::{SmsGgEmulator, SmsGgEmulatorConfig};
use snes_config::{AudioInterpolationMode, SnesAspectRatio, SnesButton};
use snes_core::api::{SnesEmulator, SnesEmulatorConfig};
use std::ffi::CStr;
use std::num::{NonZeroU32, NonZeroU64};

use ffi::{
    RETRO_DEVICE_ID_JOYPAD_A as A, RETRO_DEVICE_ID_JOYPAD_B as B,
    RETRO_DEVICE_ID_JOYPAD_DOWN as DOWN, RETRO_DEVICE_ID_JOYPAD_L as L,
    RETRO_DEVICE_ID_JOYPAD_LEFT as LEFT, RETRO_DEVICE_ID_JOYPAD_R as R,
    RETRO_DEVICE_ID_JOYPAD_RIGHT as RIGHT, RETRO_DEVICE_ID_JOYPAD_SELECT as SELECT,
    RETRO_DEVICE_ID_JOYPAD_START as START, RETRO_DEVICE_ID_JOYPAD_UP as UP,
    RETRO_DEVICE_ID_JOYPAD_X as X, RETRO_DEVICE_ID_JOYPAD_Y as Y,
};

/// Controller device ID for a Genesis 3-button pad; plain `RETRO_DEVICE_JOYPAD` is a 6-button pad
pub const GENESIS_DEVICE_3_BUTTON: u32 = retro_device_subclass(ffi::RETRO_DEVICE_JOYPAD, 0);

fn timing_option(value: &str) -> Option<TimingMode> {
    match value {
        "ntsc" => Some(TimingMode::Ntsc),
        "pal" => Some(TimingMode::Pal),
        _ => None,
    }
}

fn gamma(value: f32) -> FiniteF32 {
    FiniteF32::try_from(value).unwrap()
}

// Genesis / Sega CD / 32X

pub fn genesis_inputs() -> GenesisInputs {
    GenesisInputs {
        p1: GenesisController::new(GenesisControllerType::SixButton),
        p2: GenesisController::new(GenesisControllerType::SixButton),
    }
}

impl System for GenesisEmulator {
    const PORTS: usize = 2;

    // Same layout as Genesis Plus GX: the bottom face row (Y, B, A) is A, B, C and the shoulder
    // and top face buttons are X, Y, Z
    const BUTTONS: &'static [(u32, GenesisButton, &'static CStr)] = &[
        (UP, GenesisButton::Up, c"Up"),
        (DOWN, GenesisButton::Down, c"Down"),
        (LEFT, GenesisButton::Left, c"Left"),
        (RIGHT, GenesisButton::Right, c"Right"),
        (Y, GenesisButton::A, c"A"),
        (B, GenesisButton::B, c"B"),
        (A, GenesisButton::C, c"C"),
        (L, GenesisButton::X, c"X"),
        (X, GenesisButton::Y, c"Y"),
        (R, GenesisButton::Z, c"Z"),
        (START, GenesisButton::Start, c"Start"),
        (SELECT, GenesisButton::Mode, c"Mode"),
    ];

    const CONTROLLER_TYPES: &'static [(&'static CStr, u32)] = &[
        (c"6-Button Pad", ffi::RETRO_DEVICE_JOYPAD),
        (c"3-Button Pad", GENESIS_DEVICE_3_BUTTON),
        (c"None", ffi::RETRO_DEVICE_NONE),
    ];

    const MAX_FRAME_SIZE: (u32, u32) = (352, 480);

    fn apply_options(options: &Options, config: &mut GenesisEmulatorConfig) {
        config.forced_region = match options.get("genesis_region") {
            "americas" => Some(GenesisRegion::Americas),
            "japan" => Some(GenesisRegion::Japan),
            "europe" => Some(GenesisRegion::Europe),
            _ => None,
        };
        config.forced_timing_mode = timing_option(options.get("genesis_timing"));
        config.aspect_ratio = match options.get("genesis_aspect_ratio") {
            "ntsc" => GenesisAspectRatio::Ntsc,
            "pal" => GenesisAspectRatio::Pal,
            "square_pixels" => GenesisAspectRatio::SquarePixels,
            "stretched" => GenesisAspectRatio::Stretched,
            _ => GenesisAspectRatio::Auto,
        };
        config.remove_sprite_limits = !options.enabled("genesis_sprite_limit");
        config.render_horizontal_border = options.enabled("genesis_borders");
        config.render_vertical_border = options.enabled("genesis_borders");
        config.deinterlace = options.enabled("genesis_deinterlace");
        config.m68k_clock_divider = options.get("genesis_m68k_divider").parse().unwrap_or(7);
        config.genesis_lpf_enabled = options.enabled("genesis_low_pass");

        config.sega_cd.enable_ram_cartridge = options.enabled("scd_ram_cartridge");
        config.sega_cd.pcm_interpolation = match options.get("scd_pcm_interpolation") {
            "linear" => PcmInterpolation::Linear,
            "cubic_hermite" => PcmInterpolation::CubicHermite,
            "cubic_hermite_6point" => PcmInterpolation::CubicHermite6Point,
            _ => PcmInterpolation::None,
        };

        config.sega_32x.video_out = match options.get("32x_video_out") {
            "genesis_only" => S32XVideoOut::GenesisOnly,
            "32x_only" => S32XVideoOut::S32XOnly,
            _ => S32XVideoOut::Combined,
        };
    }

    fn set_controller(inputs: &mut GenesisInputs, port: usize, device: u32) {
        let controller_type = match device {
            ffi::RETRO_DEVICE_NONE => GenesisControllerType::None,
            GENESIS_DEVICE_3_BUTTON => GenesisControllerType::ThreeButton,
            _ => GenesisControllerType::SixButton,
        };

        let controller = match port {
            0 => &mut inputs.p1,
            1 => &mut inputs.p2,
            _ => return,
        };
        if controller.controller_type() != controller_type {
            *controller = GenesisController::new(controller_type);
        }
    }

    fn change_disc(&mut self, disc: Option<CdRom>) -> Result<(), String> {
        match disc {
            Some(disc) => GenesisEmulator::change_disc(self, disc).map_err(|err| err.to_string()),
            None => {
                self.remove_disc();
                Ok(())
            }
        }
    }
}

// Master System / Game Gear / SG-1000

pub fn smsgg_config() -> SmsGgEmulatorConfig {
    SmsGgEmulatorConfig {
        sms_timing_mode: TimingMode::default(),
        sms_model: SmsModel::default(),
        forced_psg_version: None,
        sms_aspect_ratio: SmsAspectRatio::default(),
        gg_aspect_ratio: GgAspectRatio::default(),
        remove_sprite_limit: false,
        forced_region: None,
        sms_crop_vertical_border: true,
        sms_crop_left_border: false,
        gg_frame_blending: false,
        gg_use_sms_resolution: false,
        psg_channels_enabled: [true; 4],
        fm_sound_unit_enabled: true,
        z80_divider: NonZeroU32::new(smsgg_config::NATIVE_Z80_DIVIDER).unwrap(),
        allow_opposing_joypad_directions: false,
        cheat_codes: vec![],
    }
}

impl System for SmsGgEmulator {
    const PORTS: usize = 2;

    const BUTTONS: &'static [(u32, SmsGgButton, &'static CStr)] = &[
        (UP, SmsGgButton::Up, c"Up"),
        (DOWN, SmsGgButton::Down, c"Down"),
        (LEFT, SmsGgButton::Left, c"Left"),
        (RIGHT, SmsGgButton::Right, c"Right"),
        (B, SmsGgButton::Button1, c"Button 1"),
        (A, SmsGgButton::Button2, c"Button 2"),
        (START, SmsGgButton::Pause, c"Start / Pause"),
    ];

    const MAX_FRAME_SIZE: (u32, u32) = (256, 240);

    fn apply_options(options: &Options, config: &mut SmsGgEmulatorConfig) {
        config.forced_region = match options.get("smsgg_region") {
            "international" => Some(SmsGgRegion::International),
            "domestic" => Some(SmsGgRegion::Domestic),
            _ => None,
        };
        config.sms_timing_mode = timing_option(options.get("sms_timing")).unwrap_or_default();
        config.remove_sprite_limit = !options.enabled("smsgg_sprite_limit");
        config.sms_crop_vertical_border = options.enabled("sms_crop_vertical_border");
        config.sms_crop_left_border = options.enabled("sms_crop_left_border");
        config.fm_sound_unit_enabled = options.enabled("sms_fm_unit");
        config.gg_frame_blending = options.enabled("gg_frame_blending");
    }
}

// NES

pub fn nes_config() -> NesEmulatorConfig {
    NesEmulatorConfig {
        forced_timing_mode: None,
        aspect_ratio: NesAspectRatio::default(),
        palette: NesPalette::default(),
        ntsc_crop_vertical_overscan: true,
        overscan: Overscan::default(),
        // Must be false: when true the core outputs palette indices for an NTSC shader instead
        // of RGB colors
        emulate_ntsc_output: false,
        remove_sprite_limit: false,
        pal_black_border: false,
        silence_ultrasonic_triangle_output: false,
        audio_resampler: NesAudioResampler::default(),
        audio_refresh_rate_adjustment: false,
        allow_opposing_joypad_directions: false,
        dma_dummy_joy_reads: true,
    }
}

impl System for NesEmulator {
    const PORTS: usize = 2;

    const BUTTONS: &'static [(u32, NesButton, &'static CStr)] = &[
        (UP, NesButton::Up, c"Up"),
        (DOWN, NesButton::Down, c"Down"),
        (LEFT, NesButton::Left, c"Left"),
        (RIGHT, NesButton::Right, c"Right"),
        (A, NesButton::A, c"A"),
        (B, NesButton::B, c"B"),
        (START, NesButton::Start, c"Start"),
        (SELECT, NesButton::Select, c"Select"),
    ];

    const MAX_FRAME_SIZE: (u32, u32) = (256, 240);

    fn apply_options(options: &Options, config: &mut NesEmulatorConfig) {
        config.forced_timing_mode = timing_option(options.get("nes_timing"));
        config.aspect_ratio = match options.get("nes_aspect_ratio") {
            "pal" => NesAspectRatio::Pal,
            "square_pixels" => NesAspectRatio::SquarePixels,
            "stretched" => NesAspectRatio::Stretched,
            _ => NesAspectRatio::Ntsc,
        };
        config.ntsc_crop_vertical_overscan = options.enabled("nes_crop_vertical_overscan");
        config.remove_sprite_limit = !options.enabled("nes_sprite_limit");
        config.silence_ultrasonic_triangle_output =
            options.enabled("nes_silence_ultrasonic_triangle");
    }
}

// SNES

impl System for SnesEmulator {
    const PORTS: usize = 2;

    const BUTTONS: &'static [(u32, SnesButton, &'static CStr)] = &[
        (UP, SnesButton::Up, c"Up"),
        (DOWN, SnesButton::Down, c"Down"),
        (LEFT, SnesButton::Left, c"Left"),
        (RIGHT, SnesButton::Right, c"Right"),
        (A, SnesButton::A, c"A"),
        (B, SnesButton::B, c"B"),
        (X, SnesButton::X, c"X"),
        (Y, SnesButton::Y, c"Y"),
        (L, SnesButton::L, c"L"),
        (R, SnesButton::R, c"R"),
        (START, SnesButton::Start, c"Start"),
        (SELECT, SnesButton::Select, c"Select"),
    ];

    const MAX_FRAME_SIZE: (u32, u32) = (512, 478);

    fn apply_options(options: &Options, config: &mut SnesEmulatorConfig) {
        config.forced_timing_mode = timing_option(options.get("snes_timing"));
        config.aspect_ratio = match options.get("snes_aspect_ratio") {
            "pal" => SnesAspectRatio::Pal,
            "square_pixels" => SnesAspectRatio::SquarePixels,
            "stretched" => SnesAspectRatio::Stretched,
            _ => SnesAspectRatio::Ntsc,
        };
        config.deinterlace = options.enabled("snes_deinterlace");
        config.audio_interpolation = match options.get("snes_audio_interpolation") {
            "hermite" => AudioInterpolationMode::Hermite,
            _ => AudioInterpolationMode::Gaussian,
        };
        config.gsu_overclock_factor = options
            .get("snes_gsu_overclock")
            .parse()
            .ok()
            .and_then(NonZeroU64::new)
            .unwrap_or(NonZeroU64::MIN);
    }
}

// Game Boy / Game Boy Color

pub fn gb_config() -> GameBoyEmulatorConfig {
    GameBoyEmulatorConfig {
        force_dmg_mode: false,
        force_cgb_mode: false,
        pretend_to_be_gba: false,
        aspect_ratio: GbAspectRatio::default(),
        gb_palette: GbPalette::default(),
        gb_custom_palette: [(0xFF, 0xFF, 0xFF), (0xAA, 0xAA, 0xAA), (0x55, 0x55, 0x55), (0, 0, 0)],
        gbc_color_correction: ColorCorrection::GbcLcd { screen_gamma: gamma(2.0) },
        frame_blending: true,
        audio_resampler: GbAudioResampler::default(),
        audio_60hz_hack: false,
        allow_opposing_joypad_directions: false,
    }
}

impl System for GameBoyEmulator {
    const PORTS: usize = 1;

    const BUTTONS: &'static [(u32, GameBoyButton, &'static CStr)] = &[
        (UP, GameBoyButton::Up, c"Up"),
        (DOWN, GameBoyButton::Down, c"Down"),
        (LEFT, GameBoyButton::Left, c"Left"),
        (RIGHT, GameBoyButton::Right, c"Right"),
        (A, GameBoyButton::A, c"A"),
        (B, GameBoyButton::B, c"B"),
        (START, GameBoyButton::Start, c"Start"),
        (SELECT, GameBoyButton::Select, c"Select"),
    ];

    const MAX_FRAME_SIZE: (u32, u32) = (160, 144);

    fn apply_options(options: &Options, config: &mut GameBoyEmulatorConfig) {
        config.force_dmg_mode = options.get("gb_model") == "dmg";
        config.force_cgb_mode = options.get("gb_model") == "cgb";
        config.gb_palette = match options.get("gb_palette") {
            "lime_green" => GbPalette::LimeGreen,
            "black_and_white" => GbPalette::BlackAndWhite,
            _ => GbPalette::GreenTint,
        };
        config.gbc_color_correction = match options.get("gbc_color_correction") {
            "gba_lcd" => ColorCorrection::GbaLcd { screen_gamma: gamma(3.2) },
            "none" => ColorCorrection::None,
            _ => ColorCorrection::GbcLcd { screen_gamma: gamma(2.0) },
        };
        config.frame_blending = options.enabled("gb_frame_blending");
    }
}

// Game Boy Advance

pub fn gba_config() -> GbaEmulatorConfig {
    GbaEmulatorConfig {
        color_correction: ColorCorrection::GbaLcd { screen_gamma: gamma(3.2) },
        frame_blending: true,
        aspect_ratio: GbaAspectRatio::default(),
        ..GbaEmulatorConfig::default()
    }
}

impl System for GameBoyAdvanceEmulator {
    const PORTS: usize = 1;

    const BUTTONS: &'static [(u32, GbaButton, &'static CStr)] = &[
        (UP, GbaButton::Up, c"Up"),
        (DOWN, GbaButton::Down, c"Down"),
        (LEFT, GbaButton::Left, c"Left"),
        (RIGHT, GbaButton::Right, c"Right"),
        (A, GbaButton::A, c"A"),
        (B, GbaButton::B, c"B"),
        (L, GbaButton::L, c"L"),
        (R, GbaButton::R, c"R"),
        (START, GbaButton::Start, c"Start"),
        (SELECT, GbaButton::Select, c"Select"),
    ];

    const MAX_FRAME_SIZE: (u32, u32) = (240, 160);

    fn apply_options(options: &Options, config: &mut GbaEmulatorConfig) {
        config.skip_bios_animation = options.enabled("gba_skip_bios");
        config.color_correction = match options.get("gba_color_correction") {
            "none" => ColorCorrection::None,
            _ => ColorCorrection::GbaLcd { screen_gamma: gamma(3.2) },
        };
        config.frame_blending = options.enabled("gba_frame_blending");
    }
}

// PC Engine / TurboGrafx-16

pub fn pce_config() -> PceEmulatorConfig {
    PceEmulatorConfig {
        load_disc_into_ram: false,
        always_emulate_cd_rom: false,
        region: PceRegion::default(),
        system_card_model: PceSystemCardModel::default(),
        cpu_fast_clock_divider: NonZeroU64::new(pce_config::NATIVE_FAST_CPU_DIVIDER).unwrap(),
        aspect_ratio: PceAspectRatio::default(),
        palette: PcePaletteType::default(),
        crop_overscan: true,
        remove_sprite_limits: false,
        quantize_adpcm_output: true,
        psg_audio_resampler: PcePsgResampler::default(),
        psg_channels_enabled: [true; 6],
        psg_enabled: true,
        cd_da_enabled: true,
        adpcm_enabled: true,
        psg_volume_adjustment_db: 0.0,
        cd_da_volume_adjustment_db: 0.0,
        adpcm_volume_adjustment_db: 0.0,
        input_device: PceInputDevice::default(),
        turbo_tap_connected: [true; pce_config::TURBO_TAP_GAMEPADS as usize],
        turbo_tap_gamepad_types: [PceGamepadType::default();
            pce_config::TURBO_TAP_GAMEPADS as usize],
        allow_opposing_joypad_directions: false,
        allow_simultaneous_run_select: false,
    }
}

impl System for PcEngineEmulator {
    const PORTS: usize = 5;

    // Same layout as Beetle PCE: A/B are buttons I/II, and Y/X/L/R are 6-button pad III-VI
    const BUTTONS: &'static [(u32, PceButton, &'static CStr)] = &[
        (UP, PceButton::Up, c"Up"),
        (DOWN, PceButton::Down, c"Down"),
        (LEFT, PceButton::Left, c"Left"),
        (RIGHT, PceButton::Right, c"Right"),
        (A, PceButton::Button1, c"I"),
        (B, PceButton::Button2, c"II"),
        (Y, PceButton::Button3, c"III"),
        (X, PceButton::Button4, c"IV"),
        (L, PceButton::Button5, c"V"),
        (R, PceButton::Button6, c"VI"),
        (START, PceButton::Run, c"Run"),
        (SELECT, PceButton::Select, c"Select"),
    ];

    const MAX_FRAME_SIZE: (u32, u32) = (512, 242);

    fn apply_options(options: &Options, config: &mut PceEmulatorConfig) {
        config.region = match options.get("pce_region") {
            "pc_engine" => PceRegion::PcEngine,
            _ => PceRegion::TurboGrafx16,
        };
        config.aspect_ratio = match options.get("pce_aspect_ratio") {
            "square_pixels" => PceAspectRatio::SquarePixels,
            "stretched" => PceAspectRatio::Stretched,
            _ => PceAspectRatio::Ntsc,
        };
        config.palette = match options.get("pce_palette") {
            "linear" => PcePaletteType::Linear,
            _ => PcePaletteType::PceComposite,
        };
        config.crop_overscan = options.enabled("pce_crop_overscan");
        config.remove_sprite_limits = !options.enabled("pce_sprite_limit");
        config.input_device = match options.get("pce_input_device") {
            "six_button" => PceInputDevice::SixButtonGamepad,
            "turbo_tap" => PceInputDevice::TurboTap,
            _ => PceInputDevice::TwoButtonGamepad,
        };
    }

    fn change_disc(&mut self, disc: Option<CdRom>) -> Result<(), String> {
        match disc {
            Some(disc) => PcEngineEmulator::change_disc(self, disc),
            None => self.remove_disc(),
        }
        Ok(())
    }
}
