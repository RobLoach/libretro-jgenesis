//! A running game: one jgenesis emulator plus the buffers that connect it to libretro.

use crate::env;
use crate::ffi::{RetroGameGeometry, RetroSystemAvInfo, RetroSystemTiming};
use crate::frontend::{self, AudioBuffer, SaveFiles, VideoOutput};
use crate::options::Options;
use cdrom::reader::CdRom;
use jgenesis_common::frontend::{
    ConstantInputPoller, EmulatorTrait, FrameSize, MappableInputs, TickEffect,
};
use jgenesis_common::input::Player;

pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

const PLAYERS: [Player; 5] = [Player::One, Player::Two, Player::Three, Player::Four, Player::Five];

/// Flush pending save file writes once this many frames have passed since the first one
const SAVE_FLUSH_DELAY_FRAMES: u32 = 60;

/// Safety net in case an emulator stops producing frames (e.g. a crashed game with the display
/// disabled); return to the frontend instead of hanging forever.
const MAX_TICKS_PER_FRAME: u64 = 200_000_000;

const STATE_MAGIC: &[u8; 8] = b"JGNSLRST";

/// libretro-specific behavior for each jgenesis emulator.
pub trait System: EmulatorTrait + Sized {
    /// Number of controller ports
    const PORTS: usize;

    /// Mapping from `RETRO_DEVICE_ID_JOYPAD_*` to emulator buttons
    const BUTTONS: &'static [(u32, Self::Button)];

    /// Initial maximum frame size; grown at runtime if the emulator renders a larger frame
    const MAX_FRAME_SIZE: (u32, u32);

    /// Apply core options on top of the base config built at load time
    fn apply_options(options: &Options, config: &mut Self::Config);

    /// Change the controller type for a port in response to `retro_set_controller_port_device`
    #[allow(unused_variables)]
    fn set_controller(inputs: &mut Self::Inputs, port: usize, device: u32) {}

    /// Insert or remove a disc. Only meaningful for CD-based systems.
    #[allow(unused_variables)]
    fn change_disc(&mut self, disc: Option<CdRom>) -> Result<(), String> {
        Err("This system does not support discs".into())
    }
}

/// Object-safe interface used by the libretro entry points.
pub trait CoreSession {
    fn run_frame(&mut self) -> Result<(), String>;

    fn reset(&mut self);

    fn av_info(&self) -> RetroSystemAvInfo;

    fn serialize_size(&mut self) -> usize;

    fn serialize(&mut self, out: &mut [u8]) -> bool;

    fn unserialize(&mut self, data: &[u8]) -> Result<(), String>;

    fn apply_options(&mut self, options: &Options);

    fn set_controller(&mut self, port: usize, device: u32);

    fn change_disc(&mut self, disc: Option<CdRom>) -> Result<(), String>;

    fn flush_saves(&mut self);
}

pub struct Session<E: System> {
    emulator: E,
    base_config: E::Config,
    config: E::Config,
    inputs: E::Inputs,
    video: VideoOutput,
    audio: AudioBuffer,
    saves: SaveFiles,
    frames_since_save_write: Option<u32>,
    av_info: RetroSystemAvInfo,
    state_size: usize,
}

impl<E: System> Session<E> {
    /// `create` builds the emulator from the final config (base config with options applied).
    pub fn new<F>(
        base_config: E::Config,
        inputs: E::Inputs,
        options: &Options,
        mut saves: SaveFiles,
        create: F,
    ) -> Result<Self, String>
    where
        F: FnOnce(E::Config, &mut SaveFiles) -> Result<E, String>,
    {
        let mut config = base_config.clone();
        E::apply_options(options, &mut config);

        let mut emulator = create(config.clone(), &mut saves)?;
        emulator.update_audio_output_frequency(AUDIO_SAMPLE_RATE.into());

        // Render the initial frame buffer to learn the frame size before the first retro_run
        let mut video = VideoOutput::default();
        let _ = emulator.force_render(&mut video);
        video.frame_rendered = false;

        let (max_width, max_height) = E::MAX_FRAME_SIZE;
        let size = video.size.unwrap_or(FrameSize { width: max_width, height: max_height });
        let av_info = RetroSystemAvInfo {
            geometry: RetroGameGeometry {
                base_width: size.width,
                base_height: size.height,
                max_width: max_width.max(size.width),
                max_height: max_height.max(size.height),
                aspect_ratio: aspect_ratio(size, video.pixel_aspect_ratio),
            },
            timing: RetroSystemTiming {
                fps: emulator.target_fps(),
                sample_rate: AUDIO_SAMPLE_RATE.into(),
            },
        };

        Ok(Self {
            emulator,
            base_config,
            config,
            inputs,
            video,
            audio: AudioBuffer::default(),
            saves,
            frames_since_save_write: None,
            av_info,
            state_size: 0,
        })
    }

    fn poll_inputs(&mut self) {
        env::input_poll();

        for (port, &player) in PLAYERS.iter().enumerate().take(E::PORTS) {
            let buttons = env::joypad_buttons(port as u32);
            for &(id, button) in E::BUTTONS {
                self.inputs.set_field(button, player, buttons & (1 << id) != 0);
            }
        }
    }

    /// Tell the frontend about frame size, aspect ratio, or frame rate changes.
    fn update_av_info(&mut self) {
        let Some(size) = self.video.size else { return };

        let mut av_info = self.av_info;
        av_info.geometry.base_width = size.width;
        av_info.geometry.base_height = size.height;
        av_info.geometry.aspect_ratio = aspect_ratio(size, self.video.pixel_aspect_ratio);
        av_info.timing.fps = self.emulator.target_fps();

        let grew =
            size.width > av_info.geometry.max_width || size.height > av_info.geometry.max_height;
        if grew {
            av_info.geometry.max_width = av_info.geometry.max_width.max(size.width);
            av_info.geometry.max_height = av_info.geometry.max_height.max(size.height);
        }

        if grew || (av_info.timing.fps - self.av_info.timing.fps).abs() > 1e-9 {
            env::set_system_av_info(&av_info);
        } else if av_info.geometry != self.av_info.geometry {
            env::set_geometry(&av_info.geometry);
        }

        self.av_info = av_info;
    }

    fn update_saves(&mut self) {
        if !self.saves.has_pending_writes() {
            return;
        }

        let frames = self.frames_since_save_write.get_or_insert(0);
        *frames += 1;
        if *frames >= SAVE_FLUSH_DELAY_FRAMES {
            self.saves.flush();
            self.frames_since_save_write = None;
        }
    }

    fn encode_state(&self, out: &mut Vec<u8>) -> Result<(), String> {
        let version = E::save_state_version().as_bytes();

        out.clear();
        out.extend_from_slice(STATE_MAGIC);
        out.push(version.len() as u8);
        out.extend_from_slice(version);

        let len_offset = out.len();
        out.extend_from_slice(&[0; 4]);

        let payload_offset = out.len();
        bincode::encode_into_std_write(
            self.emulator.to_save_state(),
            out,
            frontend::bincode_config(),
        )
        .map_err(|err| format!("Error encoding save state: {err}"))?;

        let payload_len = (out.len() - payload_offset) as u32;
        out[len_offset..payload_offset].copy_from_slice(&payload_len.to_le_bytes());

        Ok(())
    }
}

fn aspect_ratio(size: FrameSize, pixel_aspect_ratio: Option<f64>) -> f32 {
    let width = f64::from(size.width);
    let height = f64::from(size.height.max(1));
    match pixel_aspect_ratio {
        Some(par) => (width * par / height) as f32,
        // Stretched: let the frontend use base_width / base_height
        None => 0.0,
    }
}

impl<E: System> CoreSession for Session<E> {
    fn run_frame(&mut self) -> Result<(), String> {
        self.poll_inputs();

        self.video.frame_rendered = false;
        self.audio.samples.clear();

        let mut ticks = 0_u64;
        while !self.video.frame_rendered {
            let effect = self
                .emulator
                .tick(
                    &mut self.video,
                    &mut self.audio,
                    &mut ConstantInputPoller(&self.inputs),
                    &mut self.saves,
                )
                .map_err(|err| err.to_string())?;

            ticks += 1;
            if effect == TickEffect::FrameRendered || ticks >= MAX_TICKS_PER_FRAME {
                break;
            }
        }

        if self.video.frame_rendered
            && let Some(size) = self.video.size
        {
            self.update_av_info();
            env::video_refresh(Some(&self.video.frame), size.width, size.height);
        } else {
            env::video_refresh(
                None,
                self.av_info.geometry.base_width,
                self.av_info.geometry.base_height,
            );
        }

        env::audio_sample_batch(&self.audio.samples);

        self.update_saves();

        Ok(())
    }

    fn reset(&mut self) {
        self.emulator.soft_reset();
    }

    fn av_info(&self) -> RetroSystemAvInfo {
        self.av_info
    }

    fn serialize_size(&mut self) -> usize {
        if self.state_size == 0 {
            let mut buffer = Vec::new();
            match self.encode_state(&mut buffer) {
                // Leave headroom because encoded size can vary slightly (e.g. Option fields), and
                // frontends expect the size to stay constant for rewind and netplay
                Ok(()) => {
                    self.state_size = (buffer.len() + buffer.len() / 4).next_multiple_of(4096);
                }
                Err(err) => log::error!("{err}"),
            }
        }
        self.state_size
    }

    fn serialize(&mut self, out: &mut [u8]) -> bool {
        let mut buffer = Vec::with_capacity(out.len());
        if let Err(err) = self.encode_state(&mut buffer) {
            log::error!("{err}");
            return false;
        }

        if buffer.len() > out.len() {
            log::warn!(
                "Save state is {} bytes, larger than the {}-byte buffer; growing reported size",
                buffer.len(),
                out.len()
            );
            self.state_size = (buffer.len() + buffer.len() / 4).next_multiple_of(4096);
            return false;
        }

        out[..buffer.len()].copy_from_slice(&buffer);
        out[buffer.len()..].fill(0);
        true
    }

    fn unserialize(&mut self, data: &[u8]) -> Result<(), String> {
        let rest = data.strip_prefix(STATE_MAGIC).ok_or("Not a jgenesis save state")?;

        let (&version_len, rest) = rest.split_first().ok_or("Truncated save state")?;
        let (version, rest) =
            rest.split_at_checked(version_len.into()).ok_or("Truncated save state")?;
        if version != E::save_state_version().as_bytes() {
            return Err(format!(
                "Save state was created by a different jgenesis version ({}, expected {})",
                String::from_utf8_lossy(version),
                E::save_state_version()
            ));
        }

        let (len, rest) = rest.split_first_chunk::<4>().ok_or("Truncated save state")?;
        let payload =
            rest.get(..u32::from_le_bytes(*len) as usize).ok_or("Truncated save state")?;

        let (state, _): (E::SaveState, _) =
            bincode::decode_from_slice(payload, frontend::bincode_config())
                .map_err(|err| format!("Error decoding save state: {err}"))?;

        self.emulator.load_state(state);
        self.emulator.reload_config(&self.config);

        Ok(())
    }

    fn apply_options(&mut self, options: &Options) {
        let mut config = self.base_config.clone();
        E::apply_options(options, &mut config);
        self.emulator.reload_config(&config);
        self.config = config;
    }

    fn set_controller(&mut self, port: usize, device: u32) {
        E::set_controller(&mut self.inputs, port, device);
    }

    fn change_disc(&mut self, disc: Option<CdRom>) -> Result<(), String> {
        self.emulator.change_disc(disc)
    }

    fn flush_saves(&mut self) {
        self.saves.flush();
        self.frames_since_save_write = None;
    }
}

impl<E: System> Drop for Session<E> {
    fn drop(&mut self) {
        self.saves.flush();
    }
}
