//! InPr availability bits.
//!
//! necromancer `903633588352809b7eaf5c3cb79e8c8158ba90b1` defines this layout on
//! `InputProperties::availability3`: the high byte is source availability and
//! the low byte is mix-effect availability (LibAtem / sofie-atem-connection).
//! That commit is on necromancer `main`. This plugin stays on `feat/streamdeck`
//! (`aee784e`), which has the same `availability3` field plus AUX, DSK, and
//! protocol 2.32. These helpers decode that field with the same bit positions
//! and the same method names.

use necromancer::protocol::atom::InputProperties;
use necromancer::protocol::structs::VideoSource;
use necromancer::AtemState;

/// Where an input may be routed, other than a mix effect.
///
/// High byte of `InputProperties::availability3` (InPr offset 34).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceAvailability(u8);

impl SourceAvailability {
    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn auxiliary(self) -> bool {
        self.bit(0)
    }

    pub const fn with_auxiliary(self, on: bool) -> Self {
        self.with_bit(0, on)
    }

    pub const fn multiviewer(self) -> bool {
        self.bit(1)
    }

    pub const fn with_multiviewer(self, on: bool) -> Self {
        self.with_bit(1, on)
    }

    pub const fn super_source_art(self) -> bool {
        self.bit(2)
    }

    pub const fn with_super_source_art(self, on: bool) -> Self {
        self.with_bit(2, on)
    }

    pub const fn super_source_box(self) -> bool {
        self.bit(3)
    }

    pub const fn with_super_source_box(self, on: bool) -> Self {
        self.with_bit(3, on)
    }

    pub const fn key_source(self) -> bool {
        self.bit(4)
    }

    pub const fn with_key_source(self, on: bool) -> Self {
        self.with_bit(4, on)
    }

    /// Aux 1 output mask. Product numbering is 1-based, so this is protocol bus 0.
    pub const fn auxiliary1(self) -> bool {
        self.bit(5)
    }

    pub const fn with_auxiliary1(self, on: bool) -> Self {
        self.with_bit(5, on)
    }

    /// Aux 2 output mask. Product numbering is 1-based, so this is protocol bus 1.
    pub const fn auxiliary2(self) -> bool {
        self.bit(6)
    }

    pub const fn with_auxiliary2(self, on: bool) -> Self {
        self.with_bit(6, on)
    }

    pub const fn webcam_out(self) -> bool {
        self.bit(7)
    }

    pub const fn with_webcam_out(self, on: bool) -> Self {
        self.with_bit(7, on)
    }

    const fn bit(self, bit: u8) -> bool {
        self.0 & (1 << bit) != 0
    }

    const fn with_bit(self, bit: u8, on: bool) -> Self {
        if on {
            Self(self.0 | (1 << bit))
        } else {
            Self(self.0 & !(1 << bit))
        }
    }
}

impl From<u8> for SourceAvailability {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl From<SourceAvailability> for u8 {
    fn from(value: SourceAvailability) -> Self {
        value.0
    }
}

/// Which mix effects an input may be used on.
///
/// Low byte of `InputProperties::availability3` (InPr offset 35). M/E 1 is bit 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeAvailability(u8);

impl MeAvailability {
    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn me1(self) -> bool {
        self.bit(0)
    }

    pub const fn with_me1(self, on: bool) -> Self {
        self.with_bit(0, on)
    }

    pub const fn me2(self) -> bool {
        self.bit(1)
    }

    pub const fn with_me2(self, on: bool) -> Self {
        self.with_bit(1, on)
    }

    pub const fn me3(self) -> bool {
        self.bit(2)
    }

    pub const fn with_me3(self, on: bool) -> Self {
        self.with_bit(2, on)
    }

    pub const fn me4(self) -> bool {
        self.bit(3)
    }

    pub const fn with_me4(self, on: bool) -> Self {
        self.with_bit(3, on)
    }

    const fn bit(self, bit: u8) -> bool {
        self.0 & (1 << bit) != 0
    }

    const fn with_bit(self, bit: u8, on: bool) -> Self {
        if on {
            Self(self.0 | (1 << bit))
        } else {
            Self(self.0 & !(1 << bit))
        }
    }
}

impl From<u8> for MeAvailability {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl From<MeAvailability> for u8 {
    fn from(value: MeAvailability) -> Self {
        value.0
    }
}

/// `InputProperties::source_availability` / `me_availability` from necromancer
/// commit `90363358`, decoded from the raw `availability3` field.
pub trait InputAvailability {
    fn source_availability(&self) -> SourceAvailability;
    fn me_availability(&self) -> MeAvailability;
}

impl InputAvailability for InputProperties {
    fn source_availability(&self) -> SourceAvailability {
        SourceAvailability::from((self.availability3 >> 8) as u8)
    }

    fn me_availability(&self) -> MeAvailability {
        MeAvailability::from(self.availability3 as u8)
    }
}

/// `AtemState::get_source_availability` / `get_me_availability` from the same commit.
pub trait StateAvailability {
    fn get_source_availability(&self, source: VideoSource) -> Option<SourceAvailability>;
    fn get_me_availability(&self, source: VideoSource) -> Option<MeAvailability>;
}

impl StateAvailability for AtemState {
    fn get_source_availability(&self, source: VideoSource) -> Option<SourceAvailability> {
        self.input_properties
            .get(&source)
            .map(InputAvailability::source_availability)
    }

    fn get_me_availability(&self, source: VideoSource) -> Option<MeAvailability> {
        self.input_properties
            .get(&source)
            .map(InputAvailability::me_availability)
    }
}
