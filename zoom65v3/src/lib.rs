//! High level hidapi abstraction for interacting with zoom65v3 screen modules

use std::io::{stdout, Write};
use std::ops::Deref;
use std::sync::{LazyLock, RwLock};

use checksum::checksum;
use chrono::{DateTime, Datelike, TimeZone, Timelike};
use consts::commands;
use float::DumbFloat16;
use hidapi::{HidApi, HidDevice};
use types::ScreenPosition;

use crate::types::{Icon, Zoom65Error};

pub mod checksum;
pub mod consts;
pub mod float;
pub mod types;

/// Lazy handle to hidapi
static API: LazyLock<RwLock<HidApi>> =
    LazyLock::new(|| RwLock::new(HidApi::new().expect("failed to init hidapi")));

/// High level abstraction for managing a zoom65 v3 keyboard
pub struct Zoom65v3 {
    device: HidDevice,
    buf: [u8; 64],
}

impl Zoom65v3 {
    /// Find and open the device for modifications
    pub fn open() -> Result<Self, Zoom65Error> {
        API.write().unwrap().refresh_devices()?;
        let api = API.read().unwrap();
        let mut this = Self {
            device: api
                .device_list()
                .find(|d| {
                    d.vendor_id() == consts::ZOOM65_VENDOR_ID
                        && d.product_id() == consts::ZOOM65_PRODUCT_ID
                        && d.usage_page() == consts::ZOOM65_USAGE_PAGE
                        && d.usage() == consts::ZOOM65_USAGE
                })
                .ok_or(Zoom65Error::DeviceNotFound)?
                .open_device(&api)?,
            buf: [0u8; 64],
        };

        if !consts::APPROVED_VERSIONS.contains(&this.get_version()?) {
            return Err(Zoom65Error::UnknownFirmwareVersion);
        }
        Ok(this)
    }

    /// Get the version id tracked by the web driver
    pub fn get_version(&mut self) -> Result<u8, Zoom65Error> {
        // Write to device and read response
        self.device.write(&consts::commands::ZOOM65_VERSION_CMD)?;
        let len = self.device.read(&mut self.buf)?;
        let slice = &self.buf[..len];
        assert!(slice[0] == 1);

        // Return the version byte (at least, the one that the web driver tracks)
        Ok(slice[2])
    }

    /// Internal method to send and parse an update command
    pub fn update(&mut self, method_id: [u8; 2], slice: &[u8]) -> Result<Vec<u8>, Zoom65Error> {
        // Construct command sequence
        let mut buf = [0u8; 33];
        buf[0] = 0x0;
        buf[1] = 88;
        buf[2] = slice.len() as u8 + 3;
        buf[3] = 165;
        buf[4] = method_id[0];
        buf[5] = method_id[1];
        buf[6..6 + slice.len()].copy_from_slice(slice);

        // Write to device and read response
        self.device.write(&buf)?;
        let len = self.device.read(&mut self.buf)?;
        let slice = &self.buf[..len];
        assert!(slice[0] == 88);

        // Return result based on output code
        (slice[1] == 1 && slice[2] == 1)
            .then_some(slice.to_vec())
            .ok_or(Zoom65Error::UpdateCommandFailed)
    }

    /// Increment the screen position
    #[inline(always)]
    pub fn screen_up(&mut self) -> Result<(), Zoom65Error> {
        self.update(commands::ZOOM65_SCREEN_UP, &[])?;
        Ok(())
    }

    /// Decrement the screen position
    #[inline(always)]
    pub fn screen_down(&mut self) -> Result<(), Zoom65Error> {
        self.update(commands::ZOOM65_SCREEN_DOWN, &[])?;
        Ok(())
    }

    /// Switch the active screen
    #[inline(always)]
    pub fn screen_switch(&mut self) -> Result<(), Zoom65Error> {
        self.update(commands::ZOOM65_SCREEN_SWITCH, &[])?;
        Ok(())
    }

    /// Reset the screen back to the meletrix logo
    #[inline(always)]
    pub fn reset_screen(&mut self) -> Result<(), Zoom65Error> {
        self.update(commands::ZOOM65_RESET_SCREEN_ID, &[])?;
        Ok(())
    }

    /// Set the screen to a specific position and offset
    pub fn set_screen(&mut self, position: ScreenPosition) -> Result<(), Zoom65Error> {
        let (y, x) = position.to_directions();

        // Back to default
        self.reset_screen()?;

        // Move screen up or down
        match y {
            y if y < 0 => {
                for _ in 0..y.abs() {
                    self.screen_up()?;
                }
            },
            y if y > 0 => {
                for _ in 0..y.abs() {
                    self.screen_down()?;
                }
            },
            _ => {},
        }

        // Switch screen to offset
        for _ in 0..x {
            self.screen_switch()?;
        }

        Ok(())
    }

    /// Update the keyboards current time
    pub fn set_time<Tz: TimeZone>(&mut self, time: DateTime<Tz>) -> Result<(), Zoom65Error> {
        self.update(
            commands::ZOOM65_SET_TIME_ID,
            &[
                // Provide the current year without the century.
                // This prevents overflows on the year 2256 (meletrix web ui just subtracts 2000)
                (time.year() % 100) as u8,
                time.month() as u8,
                time.day() as u8,
                time.hour() as u8,
                time.minute() as u8,
                time.second() as u8,
            ],
        )?;
        Ok(())
    }

    /// Update the keyboards current weather report
    pub fn set_weather(
        &mut self,
        icon: Icon,
        current: u8,
        low: u8,
        high: u8,
    ) -> Result<(), Zoom65Error> {
        self.update(
            commands::ZOOM65_SET_WEATHER_ID,
            &[icon as u8, current, low, high],
        )?;
        Ok(())
    }

    /// Update the keyboards current system info
    pub fn set_system_info(
        &mut self,
        cpu_temp: u8,
        gpu_temp: u8,
        download_rate: f32,
    ) -> Result<(), Zoom65Error> {
        let bytes = DumbFloat16::new(download_rate).to_bit_repr();
        self.update(
            commands::ZOOM65_SET_SYSINFO_ID,
            &[cpu_temp, gpu_temp, bytes[0], bytes[1]],
        )?;
        Ok(())
    }

    fn upload_media(&mut self, buf: impl AsRef<[u8]>, channel: u8) -> Result<(), Zoom65Error> {
        let image = buf.as_ref();

        // start upload
        self.update(commands::ZOOM65_UPLOAD_START_ID, &[channel])?;
        self.update(
            commands::ZOOM65_UPLOAD_LENGTH,
            &(image.len() as u32).to_be_bytes(),
        )?;

        let len = image.len();
        let total = len / 24;
        let width = total.to_string().len();
        for (i, chunk) in image.chunks(24).enumerate() {
            print!("\ruploading {len} bytes ({i:width$}/{total}) ... ");
            stdout().flush().unwrap();

            let chunk_len = chunk.len();
            let mut buf = [0u8; 33];

            // command prefix
            buf[0] = 0x0;
            buf[1] = 88;
            buf[2] = 2 + chunk_len as u8 + 4;

            // chunk index and data
            buf[3] = (i >> 8) as u8;
            buf[4] = (i & 255) as u8;
            buf[5..5 + chunk.len()].copy_from_slice(chunk);

            // compute checksum
            let mut offset = 3 + 2 + chunk_len;
            if channel == 2 && i == image.len() / 24 {
                // compute padding for final payload, the checksum needs 32-bit alignment
                let padding = (4 - (image.len() % 24) % 4) % 4;
                buf[2] += padding as u8;
                offset += padding;
            }
            let data = &buf[3..offset + 2];
            let crc = checksum(data);
            buf[offset..offset + 4].copy_from_slice(&crc);

            // send payload and read response
            self.write(&buf)?;
            let len = self.device.read(&mut self.buf)?;
            let slice = &self.buf[0..len];

            if slice[1] != 1 || slice[2] != 1 {
                println!("\n{buf:?} -> \n{slice:?}");
                return Err(Zoom65Error::UpdateCommandFailed);
            }
        }

        self.update(commands::ZOOM65_UPLOAD_END, &[1])?;
        // TODO: is this required?
        self.reset_screen()?;

        println!("done");

        Ok(())
    }

    /// Upload an image to the keyboard. Must be encoded as 110x110 RGBA-3328 raw buffer
    #[inline(always)]
    pub fn upload_image(&mut self, buf: impl AsRef<[u8]>) -> Result<(), Zoom65Error> {
        self.upload_media(buf, 1)
    }

    /// Upload a gif to the keyboard. Must be 111x111.
    #[inline(always)]
    pub fn upload_gif(&mut self, buf: impl AsRef<[u8]>) -> Result<(), Zoom65Error> {
        if buf.as_ref().len() >= 1013808 {
            return Err(Zoom65Error::ImageTooLarge);
        }
        self.upload_media(buf, 2)
    }

    /// Clear the image slot
    #[inline(always)]
    pub fn clear_image(&mut self) -> Result<(), Zoom65Error> {
        self.update(commands::ZOOM65_IMAGE_DELETE, &[]).map(|_| ())
    }

    /// Clear the gif slot
    #[inline(always)]
    pub fn clear_gif(&mut self) -> Result<(), Zoom65Error> {
        self.update(commands::ZOOM65_GIF_DELETE, &[]).map(|_| ())
    }
}

impl Deref for Zoom65v3 {
    type Target = HidDevice;
    fn deref(&self) -> &Self::Target {
        &self.device
    }
}
