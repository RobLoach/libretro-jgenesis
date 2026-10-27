//! Core option definitions, registered with core options v2 when available and the legacy
//! `SET_VARIABLES` interface otherwise.

use crate::env;
use crate::ffi::{
    self, RetroCoreOptionV2Category, RetroCoreOptionV2Definition, RetroCoreOptionValue,
};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_uint};
use std::ptr;
use std::sync::OnceLock;

pub struct Category {
    pub key: &'static CStr,
    pub desc: &'static CStr,
    pub info: &'static CStr,
}

/// A core option. The first value is the default.
pub struct CoreOption {
    pub key: &'static CStr,
    pub desc: &'static CStr,
    pub info: &'static CStr,
    pub category: &'static CStr,
    pub values: &'static [(&'static CStr, &'static CStr)],
}

const ENABLED_DISABLED: &[(&CStr, &CStr)] = &[(c"enabled", c"Enabled"), (c"disabled", c"Disabled")];
const DISABLED_ENABLED: &[(&CStr, &CStr)] = &[(c"disabled", c"Disabled"), (c"enabled", c"Enabled")];
const TIMING: &[(&CStr, &CStr)] =
    &[(c"auto", c"Auto"), (c"ntsc", c"NTSC (60Hz)"), (c"pal", c"PAL (50Hz)")];

pub static CATEGORIES: &[Category] = &[
    Category {
        key: c"genesis",
        desc: c"Genesis / Mega Drive",
        info: c"Settings for Genesis, Sega CD, and 32X games.",
    },
    Category { key: c"segacd", desc: c"Sega CD", info: c"Settings specific to Sega CD games." },
    Category { key: c"32x", desc: c"32X", info: c"Settings specific to 32X games." },
    Category {
        key: c"smsgg",
        desc: c"Master System / Game Gear",
        info: c"Settings for Master System, Game Gear, and SG-1000 games.",
    },
    Category { key: c"nes", desc: c"NES", info: c"Settings for NES / Famicom games." },
    Category { key: c"snes", desc: c"SNES", info: c"Settings for SNES / Super Famicom games." },
    Category { key: c"gb", desc: c"Game Boy", info: c"Settings for Game Boy and Game Boy Color." },
    Category { key: c"gba", desc: c"Game Boy Advance", info: c"Settings for Game Boy Advance." },
    Category {
        key: c"pce",
        desc: c"PC Engine",
        info: c"Settings for PC Engine / TurboGrafx-16 HuCard and CD games.",
    },
];

pub static OPTIONS: &[CoreOption] = &[
    // Genesis
    CoreOption {
        key: c"jgenesis_genesis_region",
        desc: c"Region",
        info: c"Console region. Auto detects the region from the ROM header.",
        category: c"genesis",
        values: &[
            (c"auto", c"Auto"),
            (c"americas", c"Americas"),
            (c"japan", c"Japan"),
            (c"europe", c"Europe"),
        ],
    },
    CoreOption {
        key: c"jgenesis_genesis_timing",
        desc: c"Timing Mode",
        info: c"Force NTSC or PAL timing. Auto picks based on region.",
        category: c"genesis",
        values: TIMING,
    },
    CoreOption {
        key: c"jgenesis_genesis_aspect_ratio",
        desc: c"Aspect Ratio",
        info: c"Pixel aspect ratio reported to the frontend.",
        category: c"genesis",
        values: &[
            (c"auto", c"Auto"),
            (c"ntsc", c"NTSC"),
            (c"pal", c"PAL"),
            (c"square_pixels", c"Square Pixels"),
            (c"stretched", c"Stretched"),
        ],
    },
    CoreOption {
        key: c"jgenesis_genesis_sprite_limit",
        desc: c"Sprite Limit",
        info: c"Disabling the per-scanline sprite limit removes flicker but can break some games.",
        category: c"genesis",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_genesis_borders",
        desc: c"Render Borders",
        info: c"Render the horizontal and vertical borders around the active display.",
        category: c"genesis",
        values: DISABLED_ENABLED,
    },
    CoreOption {
        key: c"jgenesis_genesis_deinterlace",
        desc: c"Deinterlace",
        info: c"Deinterlace interlaced display modes.",
        category: c"genesis",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_genesis_m68k_divider",
        desc: c"68000 Clock Divider",
        info: c"Lower values overclock the 68000. 7 is native speed.",
        category: c"genesis",
        values: &[
            (c"7", c"7 (native)"),
            (c"6", c"6"),
            (c"5", c"5"),
            (c"4", c"4"),
            (c"3", c"3"),
            (c"2", c"2"),
            (c"1", c"1"),
        ],
    },
    CoreOption {
        key: c"jgenesis_genesis_low_pass",
        desc: c"Low-Pass Filter",
        info: c"Emulate the Model 1 Genesis audio low-pass filter.",
        category: c"genesis",
        values: ENABLED_DISABLED,
    },
    // Sega CD
    CoreOption {
        key: c"jgenesis_scd_ram_cartridge",
        desc: c"RAM Cartridge",
        info: c"Emulate a backup RAM cartridge for additional save space.",
        category: c"segacd",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_scd_pcm_interpolation",
        desc: c"PCM Interpolation",
        info: c"Interpolation used for the RF5C164 PCM chip.",
        category: c"segacd",
        values: &[
            (c"none", c"None"),
            (c"linear", c"Linear"),
            (c"cubic_hermite", c"Cubic Hermite"),
            (c"cubic_hermite_6point", c"Cubic Hermite (6-point)"),
        ],
    },
    // 32X
    CoreOption {
        key: c"jgenesis_32x_video_out",
        desc: c"Video Output",
        info: c"Which video layers to display.",
        category: c"32x",
        values: &[
            (c"combined", c"Combined"),
            (c"genesis_only", c"Genesis Only"),
            (c"32x_only", c"32X Only"),
        ],
    },
    // SMS/GG
    CoreOption {
        key: c"jgenesis_smsgg_region",
        desc: c"Region",
        info: c"International (export) or domestic (Japan) console.",
        category: c"smsgg",
        values: &[
            (c"auto", c"Auto"),
            (c"international", c"International"),
            (c"domestic", c"Japan"),
        ],
    },
    CoreOption {
        key: c"jgenesis_sms_timing",
        desc: c"Master System Timing Mode",
        info: c"NTSC or PAL timing for Master System games.",
        category: c"smsgg",
        values: &[(c"ntsc", c"NTSC (60Hz)"), (c"pal", c"PAL (50Hz)")],
    },
    CoreOption {
        key: c"jgenesis_smsgg_sprite_limit",
        desc: c"Sprite Limit",
        info: c"Disabling the per-scanline sprite limit removes flicker but can break some games.",
        category: c"smsgg",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_sms_crop_vertical_border",
        desc: c"Crop Vertical Border",
        info: c"Crop the Master System's top and bottom borders.",
        category: c"smsgg",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_sms_crop_left_border",
        desc: c"Crop Left Border",
        info: c"Crop the 8-pixel left border that many Master System games blank.",
        category: c"smsgg",
        values: DISABLED_ENABLED,
    },
    CoreOption {
        key: c"jgenesis_sms_fm_unit",
        desc: c"FM Sound Unit",
        info: c"Emulate the Japanese Master System's FM sound unit (YM2413).",
        category: c"smsgg",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_gg_frame_blending",
        desc: c"Game Gear Frame Blending",
        info: c"Blend consecutive frames to emulate the Game Gear's LCD persistence.",
        category: c"smsgg",
        values: DISABLED_ENABLED,
    },
    // NES
    CoreOption {
        key: c"jgenesis_nes_timing",
        desc: c"Timing Mode",
        info: c"Force NTSC or PAL timing. Auto uses the iNES header.",
        category: c"nes",
        values: TIMING,
    },
    CoreOption {
        key: c"jgenesis_nes_aspect_ratio",
        desc: c"Aspect Ratio",
        info: c"Pixel aspect ratio reported to the frontend.",
        category: c"nes",
        values: &[
            (c"ntsc", c"NTSC"),
            (c"pal", c"PAL"),
            (c"square_pixels", c"Square Pixels"),
            (c"stretched", c"Stretched"),
        ],
    },
    CoreOption {
        key: c"jgenesis_nes_crop_vertical_overscan",
        desc: c"Crop Vertical Overscan",
        info: c"Crop the frame from 240 to 224 lines in NTSC mode.",
        category: c"nes",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_nes_sprite_limit",
        desc: c"Sprite Limit",
        info:
            c"Disabling the 8 sprites per scanline limit removes flicker but can break some games.",
        category: c"nes",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_nes_silence_ultrasonic_triangle",
        desc: c"Silence Ultrasonic Triangle Output",
        info: c"Avoid audio popping in games that use ultrasonic triangle channel frequencies.",
        category: c"nes",
        values: DISABLED_ENABLED,
    },
    // SNES
    CoreOption {
        key: c"jgenesis_snes_timing",
        desc: c"Timing Mode",
        info: c"Force NTSC or PAL timing. Auto uses the cartridge header.",
        category: c"snes",
        values: TIMING,
    },
    CoreOption {
        key: c"jgenesis_snes_aspect_ratio",
        desc: c"Aspect Ratio",
        info: c"Pixel aspect ratio reported to the frontend.",
        category: c"snes",
        values: &[
            (c"ntsc", c"NTSC"),
            (c"pal", c"PAL"),
            (c"square_pixels", c"Square Pixels"),
            (c"stretched", c"Stretched"),
        ],
    },
    CoreOption {
        key: c"jgenesis_snes_deinterlace",
        desc: c"Deinterlace",
        info: c"Deinterlace interlaced display modes.",
        category: c"snes",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_snes_audio_interpolation",
        desc: c"Audio Interpolation",
        info: c"Gaussian matches real hardware. Hermite is sharper.",
        category: c"snes",
        values: &[(c"gaussian", c"Gaussian"), (c"hermite", c"Cubic Hermite")],
    },
    CoreOption {
        key: c"jgenesis_snes_gsu_overclock",
        desc: c"Super FX Overclock",
        info: c"Overclock the Super FX (GSU) coprocessor.",
        category: c"snes",
        values: &[(c"1", c"1x (native)"), (c"2", c"2x"), (c"3", c"3x"), (c"4", c"4x")],
    },
    // GB/GBC
    CoreOption {
        key: c"jgenesis_gb_model",
        desc: c"Hardware",
        info: c"Force Game Boy (DMG) or Game Boy Color hardware.",
        category: c"gb",
        values: &[(c"auto", c"Auto"), (c"dmg", c"Game Boy"), (c"cgb", c"Game Boy Color")],
    },
    CoreOption {
        key: c"jgenesis_gb_palette",
        desc: c"Game Boy Palette",
        info: c"Palette for monochrome Game Boy games.",
        category: c"gb",
        values: &[
            (c"green_tint", c"Green Tint"),
            (c"lime_green", c"Lime Green"),
            (c"black_and_white", c"Black and White"),
        ],
    },
    CoreOption {
        key: c"jgenesis_gbc_color_correction",
        desc: c"Game Boy Color Color Correction",
        info: c"Emulate the colors of the Game Boy Color or Game Boy Advance LCD.",
        category: c"gb",
        values: &[
            (c"gbc_lcd", c"Game Boy Color LCD"),
            (c"gba_lcd", c"Game Boy Advance LCD"),
            (c"none", c"None"),
        ],
    },
    CoreOption {
        key: c"jgenesis_gb_frame_blending",
        desc: c"Frame Blending",
        info: c"Blend consecutive frames to emulate LCD persistence.",
        category: c"gb",
        values: ENABLED_DISABLED,
    },
    // GBA
    CoreOption {
        key: c"jgenesis_gba_skip_bios",
        desc: c"Skip BIOS Animation",
        info: c"Skip the BIOS boot animation.",
        category: c"gba",
        values: DISABLED_ENABLED,
    },
    CoreOption {
        key: c"jgenesis_gba_color_correction",
        desc: c"Color Correction",
        info: c"Emulate the colors of the Game Boy Advance LCD.",
        category: c"gba",
        values: &[(c"gba_lcd", c"Game Boy Advance LCD"), (c"none", c"None")],
    },
    CoreOption {
        key: c"jgenesis_gba_frame_blending",
        desc: c"Frame Blending",
        info: c"Blend consecutive frames to emulate LCD persistence.",
        category: c"gba",
        values: ENABLED_DISABLED,
    },
    // PC Engine
    CoreOption {
        key: c"jgenesis_pce_region",
        desc: c"Region",
        info: c"TurboGrafx-16 (US) or PC Engine (Japan).",
        category: c"pce",
        values: &[(c"turbografx16", c"TurboGrafx-16"), (c"pc_engine", c"PC Engine")],
    },
    CoreOption {
        key: c"jgenesis_pce_aspect_ratio",
        desc: c"Aspect Ratio",
        info: c"Pixel aspect ratio reported to the frontend.",
        category: c"pce",
        values: &[
            (c"ntsc", c"NTSC"),
            (c"square_pixels", c"Square Pixels"),
            (c"stretched", c"Stretched"),
        ],
    },
    CoreOption {
        key: c"jgenesis_pce_palette",
        desc: c"Palette",
        info: c"Composite emulates the colors of a real console's composite video output.",
        category: c"pce",
        values: &[(c"composite", c"Composite"), (c"linear", c"Linear RGB")],
    },
    CoreOption {
        key: c"jgenesis_pce_crop_overscan",
        desc: c"Crop Overscan",
        info: c"Crop the overscan area.",
        category: c"pce",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_pce_sprite_limit",
        desc: c"Sprite Limit",
        info: c"Disabling the per-scanline sprite limit removes flicker but can break some games.",
        category: c"pce",
        values: ENABLED_DISABLED,
    },
    CoreOption {
        key: c"jgenesis_pce_input_device",
        desc: c"Input Device",
        info: c"Controller type. TurboTap enables up to 5 players.",
        category: c"pce",
        values: &[
            (c"two_button", c"2-Button Gamepad"),
            (c"six_button", c"6-Button Gamepad"),
            (c"turbo_tap", c"TurboTap (5 players)"),
        ],
    },
];

/// Owns the C structures handed to the frontend so their pointers stay valid.
struct Registered {
    _categories: Vec<RetroCoreOptionV2Category>,
    definitions: Vec<RetroCoreOptionV2Definition>,
    categories_ptr: *const RetroCoreOptionV2Category,
    _legacy_strings: Vec<CString>,
    legacy: Vec<ffi::RetroVariable>,
}

// SAFETY: Only contains pointers to 'static data and heap allocations owned by this struct
unsafe impl Send for Registered {}
// SAFETY: Never mutated after construction
unsafe impl Sync for Registered {}

static REGISTERED: OnceLock<Registered> = OnceLock::new();

fn build() -> Registered {
    let mut categories: Vec<_> = CATEGORIES
        .iter()
        .map(|category| RetroCoreOptionV2Category {
            key: category.key.as_ptr(),
            desc: category.desc.as_ptr(),
            info: category.info.as_ptr(),
        })
        .collect();
    categories.push(RetroCoreOptionV2Category {
        key: ptr::null(),
        desc: ptr::null(),
        info: ptr::null(),
    });

    let null_value = RetroCoreOptionValue { value: ptr::null(), label: ptr::null() };
    let mut definitions: Vec<_> = OPTIONS
        .iter()
        .map(|option| {
            assert!(option.values.len() < ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX);

            let mut values = [null_value; ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX];
            for (slot, &(value, label)) in values.iter_mut().zip(option.values) {
                *slot = RetroCoreOptionValue { value: value.as_ptr(), label: label.as_ptr() };
            }

            RetroCoreOptionV2Definition {
                key: option.key.as_ptr(),
                desc: option.desc.as_ptr(),
                desc_categorized: ptr::null(),
                info: option.info.as_ptr(),
                info_categorized: ptr::null(),
                category_key: option.category.as_ptr(),
                values,
                default_value: option.values[0].0.as_ptr(),
            }
        })
        .collect();
    definitions.push(RetroCoreOptionV2Definition {
        key: ptr::null(),
        desc: ptr::null(),
        desc_categorized: ptr::null(),
        info: ptr::null(),
        info_categorized: ptr::null(),
        category_key: ptr::null(),
        values: [null_value; ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX],
        default_value: ptr::null(),
    });

    // Legacy format: "Description; default|other|values"
    let legacy_strings: Vec<CString> = OPTIONS
        .iter()
        .map(|option| {
            let category =
                CATEGORIES.iter().find(|category| category.key == option.category).unwrap();
            let values: Vec<_> =
                option.values.iter().map(|(value, _)| value.to_str().unwrap()).collect();
            CString::new(format!(
                "{} > {}; {}",
                category.desc.to_str().unwrap(),
                option.desc.to_str().unwrap(),
                values.join("|")
            ))
            .unwrap()
        })
        .collect();
    let mut legacy: Vec<_> = OPTIONS
        .iter()
        .zip(&legacy_strings)
        .map(|(option, value)| ffi::RetroVariable {
            key: option.key.as_ptr(),
            value: value.as_ptr(),
        })
        .collect();
    legacy.push(ffi::RetroVariable { key: ptr::null(), value: ptr::null() });

    let categories_ptr = categories.as_ptr();
    Registered {
        _categories: categories,
        definitions,
        categories_ptr,
        _legacy_strings: legacy_strings,
        legacy,
    }
}

/// Register all core options with the frontend. Must be called from `retro_set_environment`.
pub fn register() {
    let registered = REGISTERED.get_or_init(build);

    let mut version: c_uint = 0;
    // SAFETY: GET_CORE_OPTIONS_VERSION takes an unsigned*
    let has_version = unsafe {
        env::environment(ffi::RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION, (&raw mut version).cast())
    };

    if has_version && version >= 2 {
        let mut options = ffi::RetroCoreOptionsV2 {
            categories: registered.categories_ptr,
            definitions: registered.definitions.as_ptr(),
        };
        // SAFETY: SET_CORE_OPTIONS_V2 takes a const retro_core_options_v2*
        let ok = unsafe {
            env::environment(ffi::RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2, (&raw mut options).cast())
        };
        if ok {
            return;
        }
    }

    // SAFETY: SET_VARIABLES takes a NULL-terminated const retro_variable array
    unsafe {
        env::environment(
            ffi::RETRO_ENVIRONMENT_SET_VARIABLES,
            registered.legacy.as_ptr().cast_mut().cast(),
        );
    }
}

/// Current values of all core options.
pub struct Options {
    values: HashMap<&'static str, String>,
}

impl Options {
    pub fn read() -> Self {
        let values = OPTIONS
            .iter()
            .map(|option| {
                let key = option.key.to_str().unwrap();
                let value = env::get_variable(option.key)
                    .filter(|value| option.values.iter().any(|(v, _)| v.to_str() == Ok(value)))
                    .unwrap_or_else(|| option.values[0].0.to_str().unwrap().into());
                (key, value)
            })
            .collect();
        Self { values }
    }

    /// Value of an option, without the `jgenesis_` prefix.
    pub fn get(&self, key: &str) -> &str {
        let full_key = format!("jgenesis_{key}");
        self.values.get(full_key.as_str()).map_or("", String::as_str)
    }

    pub fn enabled(&self, key: &str) -> bool {
        self.get(key) == "enabled"
    }
}
