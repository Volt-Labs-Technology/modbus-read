//! Register words into numbers.
//!
//! A device answers with 16-bit words. Whether two of them are 100 000 or
//! 50.0 comes from its manual. [`RegisterKind`] is that choice.
//! [`Scale`] is the factor the map applies afterwards.

use crate::frame::{ModbusError, RegisterCount};

/// How a device's register map spells one quantity.
///
/// Registers are 16-bit words and mean nothing by themselves: the same two
/// words are 100 000 to one meter and 50.0 to the next. Which it is comes from
/// the device's manual and is decided here — once, as a calculation over the
/// words, so no caller reassembles bytes.
///
/// Four kinds, because four are what common meter manuals print. A fifth
/// arrives with the device that needs it, as a variant the compiler makes
/// every `match` account for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegisterKind {
    /// One register as an unsigned 16-bit integer.
    U16,
    /// One register as a signed 16-bit integer.
    I16,
    /// Two registers as an unsigned 32-bit integer, most significant word first.
    U32Be,
    /// Two registers as an IEEE 754 binary32, most significant word first.
    F32Be,
}

/// The factor a register map applies to a decoded word.
///
/// A meter that reports tenths of a unit is read with a scale of 0.1. The
/// constructor is the check: zero would read every register as nothing and no
/// map means that, and a non-finite factor would put a NaN in a reading. A
/// *negative* scale is accepted — a device whose export register counts down
/// from zero is a map, not a mistake.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Scale(f64);

impl RegisterKind {
    /// How many registers this kind occupies.
    ///
    /// Exhaustive by construction, like [`crate::FunctionCode::code`]: a
    /// fifth kind does not compile until someone writes here how wide it is.
    #[must_use]
    pub const fn registers(self) -> u16 {
        match self {
            Self::U16 | Self::I16 => 1,
            Self::U32Be | Self::F32Be => 2,
        }
    }

    /// Whether a read of `count` registers is the width this kind needs.
    ///
    /// The one spelling of that rule, so every register map asks the same
    /// question instead of writing the comparison again: a `U32Be` read as one
    /// register is half a number.
    #[must_use]
    pub const fn fits(self, count: RegisterCount) -> bool {
        count.get() == self.registers()
    }

    /// The kind's name, for an error an operator reads beside a manual.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::U16 => "u16",
            Self::I16 => "i16",
            Self::U32Be => "u32be",
            Self::F32Be => "f32be",
        }
    }

    /// The number these words carry, unscaled.
    ///
    /// Big-endian throughout, first word most significant, which is the
    /// protocol's order and the order most device manuals print.
    ///
    /// # Errors
    ///
    /// Returns [`ModbusError::WordCount`] when the slice is not exactly
    /// [`Self::registers`] words long. A `U32Be` decoded from one word would
    /// be half a number.
    pub fn decode(self, words: &[u16]) -> Result<f64, ModbusError> {
        match (self, words) {
            (Self::U16, [word]) => Ok(f64::from(*word)),
            (Self::I16, [word]) => Ok(f64::from(i16::from_be_bytes(word.to_be_bytes()))),
            (Self::U32Be, [high, low]) => Ok(f64::from(wide(*high, *low))),
            // IEEE 754 single precision, the two words in the order the device
            // sent them. Every bit pattern is an `f32`.
            (Self::F32Be, [high, low]) => Ok(f64::from(f32::from_bits(wide(*high, *low)))),
            // Every kind is named again rather than swept into a `_`, so a
            // fifth one does not compile until someone says how wide it is.
            // The only case left here is a slice of the wrong length.
            (Self::U16 | Self::I16 | Self::U32Be | Self::F32Be, _) => Err(ModbusError::WordCount {
                kind: self.name(),
                expected: self.registers(),
                found: words.len(),
            }),
        }
    }
}

/// Two registers as one 32-bit value, the first register most significant.
fn wide(high: u16, low: u16) -> u32 {
    (u32::from(high) << u16::BITS) | u32::from(low)
}

impl Scale {
    /// A finite, non-zero factor.
    ///
    /// # Errors
    ///
    /// Returns [`ModbusError::NonFiniteScale`] for NaN or an infinity and
    /// [`ModbusError::ZeroScale`] for zero.
    pub fn new(factor: f64) -> Result<Self, ModbusError> {
        if !factor.is_finite() {
            return Err(ModbusError::NonFiniteScale);
        }
        if factor == 0.0 {
            return Err(ModbusError::ZeroScale);
        }
        Ok(Self(factor))
    }

    /// A map that changes nothing: the register is already in the unit
    /// the caller wants.
    #[must_use]
    pub const fn one() -> Self {
        Self(1.0)
    }

    /// The factor itself.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// This scale applied to a decoded register.
    #[must_use]
    pub fn apply(self, value: f64) -> f64 {
        value * self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{RegisterKind, Scale};
    use crate::frame::{ModbusError, RegisterCount};

    /// Every kind decoded here is compared exactly. These are integers and
    /// powers of two in `f64`, so an exact comparison is the right one and a
    /// tolerance would only hide a decoder that drifted.
    ///
    /// The bit patterns are synthetic. They are not readings from a device.
    #[test]
    fn a_float_register_pair_decodes_to_the_number_its_manual_prints() {
        let decoded = RegisterKind::F32Be.decode(&[0x4248, 0x0000]);

        assert_eq!(decoded, Ok(50.0));
    }

    #[test]
    fn a_signed_register_decodes_below_zero() {
        let decoded = RegisterKind::I16.decode(&[0xFF38]);

        assert_eq!(decoded, Ok(-200.0));
    }

    #[test]
    fn the_same_word_is_two_numbers_depending_on_the_kind() {
        assert_eq!(RegisterKind::U16.decode(&[0xFFFF]), Ok(65_535.0));
        assert_eq!(RegisterKind::I16.decode(&[0xFFFF]), Ok(-1.0));
    }

    #[test]
    fn a_wide_register_pair_decodes_most_significant_word_first() {
        let decoded = RegisterKind::U32Be.decode(&[0x0001, 0x86A0]);

        assert_eq!(decoded, Ok(100_000.0));
    }

    #[test]
    fn each_kind_knows_how_many_registers_it_occupies() {
        assert_eq!(RegisterKind::U16.registers(), 1);
        assert_eq!(RegisterKind::I16.registers(), 1);
        assert_eq!(RegisterKind::U32Be.registers(), 2);
        assert_eq!(RegisterKind::F32Be.registers(), 2);
    }

    #[test]
    fn each_kind_names_itself() {
        assert_eq!(RegisterKind::U16.name(), "u16");
        assert_eq!(RegisterKind::I16.name(), "i16");
        assert_eq!(RegisterKind::U32Be.name(), "u32be");
        assert_eq!(RegisterKind::F32Be.name(), "f32be");
    }

    /// Every register map asks this one question, so it is proved once here
    /// rather than in each of them.
    #[test]
    fn a_count_fits_only_the_kind_that_is_that_wide() {
        let one = RegisterCount::new(1).expect("one register is a count");
        let two = RegisterCount::new(2).expect("two registers is a count");

        assert!(RegisterKind::I16.fits(one));
        assert!(!RegisterKind::I16.fits(two));
        assert!(RegisterKind::F32Be.fits(two));
        assert!(!RegisterKind::F32Be.fits(one));
    }

    /// Half of a 32-bit quantity is not a smaller reading, it is a wrong one.
    #[test]
    fn a_wide_kind_handed_one_word_is_refused() {
        let refused = RegisterKind::U32Be.decode(&[0x0001]);

        assert_eq!(
            refused,
            Err(ModbusError::WordCount {
                kind: "u32be",
                expected: 2,
                found: 1,
            })
        );
    }

    #[test]
    fn a_narrow_kind_handed_two_words_is_refused() {
        let refused = RegisterKind::U16.decode(&[1, 2]);

        assert_eq!(
            refused,
            Err(ModbusError::WordCount {
                kind: "u16",
                expected: 1,
                found: 2,
            })
        );
    }

    #[test]
    fn a_kind_handed_no_words_is_refused() {
        let refused = RegisterKind::F32Be.decode(&[]);

        assert_eq!(
            refused,
            Err(ModbusError::WordCount {
                kind: "f32be",
                expected: 2,
                found: 0,
            })
        );
    }

    /// Scaled readings are compared with a tolerance rather than for
    /// equality: 0.1 is not exact in binary, and a test that depended on the
    /// last bit would be asserting the rounding mode rather than the decoder.
    fn near(found: f64, expected: f64) -> bool {
        (found - expected).abs() < 1e-9
    }

    #[test]
    fn a_scale_turns_tenths_into_the_unit() {
        let scale = Scale::new(0.1).expect("a tenth is a scale");

        let scaled = scale.apply(2500.0);

        assert!(near(scaled, 250.0), "2500 tenths is 250, found {scaled}");
        assert!(near(scale.get(), 0.1));
    }

    #[test]
    fn a_scale_of_one_changes_nothing() {
        assert!(near(Scale::one().apply(412.0), 412.0));
    }

    /// A device whose export register counts down from zero is a map, not a
    /// mistake.
    #[test]
    fn a_negative_scale_is_a_map_and_is_accepted() {
        let scale = Scale::new(-1.0).expect("a sign flip is a scale");

        assert!(near(scale.apply(250.0), -250.0));
    }

    #[test]
    fn a_scale_of_zero_is_refused() {
        assert_eq!(Scale::new(0.0), Err(ModbusError::ZeroScale));
    }

    #[test]
    fn a_scale_that_is_not_a_number_is_refused() {
        assert_eq!(Scale::new(f64::NAN), Err(ModbusError::NonFiniteScale));
        assert_eq!(Scale::new(f64::INFINITY), Err(ModbusError::NonFiniteScale));
        assert_eq!(
            Scale::new(f64::NEG_INFINITY),
            Err(ModbusError::NonFiniteScale)
        );
    }
}
