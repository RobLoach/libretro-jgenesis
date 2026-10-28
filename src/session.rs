//! A running game: one jgenesis emulator plus the buffers that connect it to libretro.

use crate::env;
use crate::ffi::{
    self, RetroControllerDescription, RetroControllerInfo, RetroGameGeometry, RetroInputDescriptor,
    RetroSystemAvInfo, RetroSystemTiming,
};
use crate::frontend::{self, AudioBuffer, SaveFiles, VideoOutput};
use crate::options::Options;
use cdrom::reader::CdRom;
use jgenesis_common::frontend::{
    ConstantInputPoller, EmulatorTrait, FrameSize, MappableInputs, TickEffect,
};
use jgenesis_common::input::Player;
use std::ffi::{CStr, c_uint};
use std::ptr;

/// Output sample rate when the frontend doesn't report its own (`GET_TARGET_SAMPLE_RATE`)
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

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

    /// Mapping from `RETRO_DEVICE_ID_JOYPAD_*` to emulator buttons, with the button's label
    const BUTTONS: &'static [(u32, Self::Button, &'static CStr)];

    /// Controller types offered on every port; only systems that implement `set_controller` should
    /// offer more than one
    const CONTROLLER_TYPES: &'static [(&'static CStr, u32)] =
        &[(c"Gamepad", ffi::RETRO_DEVICE_JOYPAD)];

    /// Initial maximum frame size; grown at runtime if the emulator renders a larger frame
    const MAX_FRAME_SIZE: (u32, u32);

    /// Whether the console has a reset button. Without one, jgenesis's `soft_reset` does nothing,
    /// so `retro_reset` power cycles instead.
    const HAS_SOFT_RESET: bool = true;

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
        let sample_rate = env::target_sample_rate().unwrap_or(DEFAULT_SAMPLE_RATE);
        emulator.update_audio_output_frequency(sample_rate.into());

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
                sample_rate: sample_rate.into(),
            },
        };

        Self::register_inputs();

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

    /// Tell the frontend each port's button labels and controller types. The frontend copies the
    /// arrays.
    fn register_inputs() {
        let descriptors: Vec<_> = (0..E::PORTS as c_uint)
            .flat_map(|port| {
                E::BUTTONS.iter().map(move |&(id, _, label)| RetroInputDescriptor {
                    port,
                    device: ffi::RETRO_DEVICE_JOYPAD,
                    index: 0,
                    id,
                    description: label.as_ptr(),
                })
            })
            .chain([RetroInputDescriptor {
                port: 0,
                device: 0,
                index: 0,
                id: 0,
                description: ptr::null(),
            }])
            .collect();
        // SAFETY: SET_INPUT_DESCRIPTORS takes a NULL-terminated retro_input_descriptor array
        unsafe {
            env::environment(
                ffi::RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS,
                descriptors.as_ptr().cast_mut().cast(),
            );
        }

        let types: Vec<_> = E::CONTROLLER_TYPES
            .iter()
            .map(|&(desc, id)| RetroControllerDescription { desc: desc.as_ptr(), id })
            .collect();
        let ports: Vec<_> = (0..E::PORTS)
            .map(|_| RetroControllerInfo {
                types: types.as_ptr(),
                num_types: types.len() as c_uint,
            })
            .chain([RetroControllerInfo { types: ptr::null(), num_types: 0 }])
            .collect();
        // SAFETY: SET_CONTROLLER_INFO takes a NULL-terminated retro_controller_info array
        unsafe {
            env::environment(
                ffi::RETRO_ENVIRONMENT_SET_CONTROLLER_INFO,
                ports.as_ptr().cast_mut().cast(),
            );
        }
    }

    fn poll_inputs(&mut self) {
        env::input_poll();

        // Ports beyond what the frontend has devices for read as released, without polling
        let active_ports = env::input_max_users().unwrap_or(E::PORTS);
        for (port, &player) in PLAYERS.iter().enumerate().take(E::PORTS) {
            let buttons = if port < active_ports { env::joypad_buttons(port as u32) } else { 0 };
            for &(id, button, _) in E::BUTTONS {
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

    fn restore_audio_output_frequency(&mut self) {
        self.emulator.update_audio_output_frequency(self.av_info.timing.sample_rate as u64);
    }

    /// Save state header: magic, then the length-prefixed save state version string.
    /// The u32 payload length follows the header.
    fn state_header() -> Vec<u8> {
        let version = E::save_state_version().as_bytes();
        let mut header = Vec::with_capacity(STATE_MAGIC.len() + 1 + version.len());
        header.extend_from_slice(STATE_MAGIC);
        header.push(version.len() as u8);
        header.extend_from_slice(version);
        header
    }

    /// Encode the current state directly into `out`, returning the number of bytes written, or
    /// None if `out` is too small.
    fn encode_state_into(&self, out: &mut [u8]) -> Result<Option<usize>, String> {
        let header = Self::state_header();
        let payload_offset = header.len() + 4;
        if out.len() < payload_offset {
            return Ok(None);
        }

        let payload_len = match bincode::encode_into_slice(
            self.emulator.to_save_state(),
            &mut out[payload_offset..],
            frontend::bincode_config(),
        ) {
            Ok(len) => len,
            Err(bincode::error::EncodeError::UnexpectedEnd) => return Ok(None),
            Err(err) => return Err(format!("Error encoding save state: {err}")),
        };

        out[..header.len()].copy_from_slice(&header);
        out[header.len()..payload_offset].copy_from_slice(&(payload_len as u32).to_le_bytes());

        Ok(Some(payload_offset + payload_len))
    }

    /// Total encoded size of the current state, including the header.
    fn encoded_state_len(&self) -> Result<usize, String> {
        let mut writer = bincode::enc::write::SizeWriter::default();
        bincode::encode_into_writer(
            self.emulator.to_save_state(),
            &mut writer,
            frontend::bincode_config(),
        )
        .map_err(|err| format!("Error encoding save state: {err}"))?;

        Ok(Self::state_header().len() + 4 + writer.bytes_written)
    }

    /// Reported state size: leave headroom because the encoded size can vary slightly (e.g.
    /// Option fields), and frontends expect it to stay constant for rewind and netplay.
    fn padded_state_size(len: usize) -> usize {
        (len + len / 4).next_multiple_of(4096)
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

        let av_enable = env::audio_video_enable();
        self.video.skip_output = !av_enable.video;
        self.audio.discard = !av_enable.audio;

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

        if self.video.frame_rendered {
            self.update_av_info();
        }

        // Send a new frame, or repeat the last one: as a dupe (NULL) if the frontend supports it,
        // otherwise by sending the last frame's pixels again
        let new_frame = self.video.frame_rendered && !self.video.skip_output;
        match self.video.frame_size {
            Some(size) if new_frame || !env::can_dupe() => {
                env::video_refresh(Some(&self.video.frame), size.width, size.height);
            }
            _ => env::video_refresh(
                None,
                self.av_info.geometry.base_width,
                self.av_info.geometry.base_height,
            ),
        }

        env::audio_sample_batch(&self.audio.samples, &mut self.audio.int16_samples);

        self.update_saves();

        Ok(())
    }

    fn reset(&mut self) {
        if E::HAS_SOFT_RESET {
            self.emulator.soft_reset();
        } else {
            self.emulator.hard_reset(&mut self.saves);
            // Hard reset rebuilds the emulator at its default output rate
            self.restore_audio_output_frequency();
        }
    }

    fn av_info(&self) -> RetroSystemAvInfo {
        self.av_info
    }

    fn serialize_size(&mut self) -> usize {
        if self.state_size == 0 {
            match self.encoded_state_len() {
                Ok(len) => self.state_size = Self::padded_state_size(len),
                Err(err) => log::error!("{err}"),
            }
        }
        self.state_size
    }

    fn serialize(&mut self, out: &mut [u8]) -> bool {
        match self.encode_state_into(out) {
            Ok(Some(len)) => {
                out[len..].fill(0);
                true
            }
            Ok(None) => {
                // Grow the reported size so the frontend's next buffer is large enough
                let len = self.encoded_state_len().unwrap_or(out.len() * 2);
                log::warn!(
                    "Save state is {len} bytes, larger than the {}-byte buffer; growing reported size",
                    out.len()
                );
                self.state_size = Self::padded_state_size(len);
                false
            }
            Err(err) => {
                log::error!("{err}");
                false
            }
        }
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
