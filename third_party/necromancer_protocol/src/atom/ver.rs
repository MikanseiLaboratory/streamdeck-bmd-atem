//! # Version and product information; 2/3 atoms
//!
//! ## Unimplemented atoms (1)
//!
//! FourCC | Atom name | Length
//! ------ | --------- | ------
//! `SwVr` | `SoftwareVersion` | 0x28

use crate::{atom::str_from_utf8_null, error::Error, Result};
use binrw::binrw;
use std::fmt::Display;

/// `_ver`: protocol version (`CapabilitiesVersion`)
///
/// Upstream necromancer documents 2.30 as the supported firmware. This vendored
/// copy also accepts 2.31 and 2.32. Other versions still fail
/// [`Version::check_firmware_version`].
///
/// ## Packet format
///
/// * `u16`: [major version](Self::major)
/// * `u16`: [minor version](Self::minor)
#[binrw]
#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
#[brw(big)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

impl Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}.{}", self.major, self.minor))
    }
}

impl Version {
    pub fn check_firmware_version(&self) -> Result<()> {
        // Upstream necromancer accepts 2.30 and 2.31 only (cut/fade on 2.31 is still a FIXME).
        // 2.32 uses the same commands this plugin sends, so it is allowed through the same gate.
        // A later minor, or any other major, still fails closed.
        if self.major != 2 || self.minor < 30 || self.minor > 32 {
            return Err(Error::UnsupportedFirmwareVersion(*self));
        }

        Ok(())
    }
}

/// `_pin`: Product info (`CapabilitiesProductInfo`)
///
/// ## Packet format
///
/// * `char[40]`: product name, as a UTF-8 encoded, null-padded string.
/// * `u8`: product ID
/// * 3 bytes padding (null)
#[binrw]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[brw(big)]
pub struct ProductName {
    #[br(try_map = |v: [u8; Self::MAX_NAME_LENGTH]| str_from_utf8_null(&v).map(str::to_string))]
    #[bw(assert(name.len() <= Self::MAX_NAME_LENGTH), pad_size_to = Self::MAX_NAME_LENGTH, map = |v: &String| { v.as_bytes().to_vec() })]
    name: String,
    #[brw(pad_after = 3)]
    id: u8,
}

impl ProductName {
    const LENGTH: usize = 44;
    const MAX_NAME_LENGTH: usize = Self::LENGTH - 4;

    pub fn new(name: String, id: u8) -> Result<Self> {
        if name.len() > Self::MAX_NAME_LENGTH {
            return Err(Error::InvalidLength);
        }

        Ok(Self { name, id })
    }

    /// Get the human-readable product name.
    #[inline]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the product ID
    #[inline]
    pub fn id(&self) -> u8 {
        self.id
    }

    /// Returns `true` if the device supports simultaneous ISO recording of all
    /// inputs.
    pub fn supports_iso_recording_all_inputs(&self) -> bool {
        matches!(self.id, 0xf | 0x11 | 0x16 | 0x17 | 0x1b)
    }

    /// Returns `true` if the device supports streaming over RTMP.
    pub fn supports_rtmp_streaming(&self) -> bool {
        matches!(self.id, 0xe | 0xf | 0x10 | 0x11 | 0x16 | 0x17 | 0x1a | 0x1b)
    }

    /// Returns `true` if the device supports recording to media.
    pub fn supports_recording(&self) -> bool {
        matches!(self.id, 0xe | 0xf | 0x10 | 0x11 | 0x16 | 0x17 | 0x1a | 0x1b)
    }

    pub fn supports_remote_source(&self) -> bool {
        self.id == 0x1b
    }

    pub fn big_endian_audio(&self) -> bool {
        matches!(self.id, 0x1..=0x3)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::atom::{Atom, Payload};
    use binrw::{BinRead, BinWrite};
    use std::io::Cursor;

    #[test]
    fn ver() -> Result<()> {
        let expected = Version {
            major: 2,
            minor: 30,
        };
        let cmd = hex::decode("000c00005f7665720002001e")?;
        let ver = Atom::read(&mut Cursor::new(&cmd))?;
        let Payload::Version(version) = ver.payload else {
            panic!("wrong command type");
        };
        assert_eq!(expected, version);

        let o = Atom::new(expected);
        let mut out = Cursor::new(Vec::with_capacity(cmd.len()));
        o.write(&mut out)?;
        assert_eq!(cmd, out.into_inner());
        Ok(())
    }

    #[test]
    fn pin() -> Result<()> {
        let expected = ProductName::new("ATEM Mini".to_string(), 0xd)?;
        let cmd = hex::decode(concat!(
            "00340000",
            "5f70696e",
            "4154454d204d696e6900000000000000000000000000000000000000000000000000000000000000",
            "0d000000",
        ))?;
        let pin = Atom::read(&mut Cursor::new(&cmd))?;
        let Payload::ProductName(name) = pin.payload else {
            panic!("wrong command type");
        };
        assert_eq!(expected, name);
        assert_eq!("ATEM Mini", name.name());
        assert_eq!(0xd, name.id());
        assert!(!name.supports_iso_recording_all_inputs());
        assert!(!name.supports_rtmp_streaming());
        assert!(!name.supports_recording());

        let o = Atom::new(expected);
        let mut out = Cursor::new(Vec::with_capacity(cmd.len()));
        o.write(&mut out)?;
        assert_eq!(cmd, out.into_inner());
        Ok(())
    }
}
