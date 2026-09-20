//! Modbus TCP frames: the MBAP header plus the two read PDUs.
//!
//! Building a frame is arithmetic on a dozen bytes. [`ReadRequest::encode`]
//! turns five values into a frame and [`ReadRequest::decode`] turns a frame
//! back into register words. Neither opens a socket.

use std::fmt;

/// What a Modbus read refuses, on the way out and on the way back.
///
/// Every variant names what was expected beside what was found, because the
/// reader of this error is an operator holding a register map and a device
/// manual, and "length mismatch" alone sends them to a packet capture.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModbusError {
    /// Unit id 0 is a broadcast. No device answers a broadcast read.
    #[error("unit id 0 is a broadcast, which no device answers; a read needs an addressed unit")]
    BroadcastUnit,
    /// Unit ids 248 to 254 are reserved by the specification.
    #[error("unit id {unit} is reserved by the specification (248-254)")]
    ReservedUnit {
        /// The reserved unit id that was refused.
        unit: u8,
    },
    /// A read of zero registers asks a device for nothing.
    #[error("a read of no registers asks a device for nothing")]
    EmptyRead,
    /// Function codes 3 and 4 answer at most [`MAX_REGISTERS`] words.
    #[error("a read of {count} registers is above the {max} this function code can answer")]
    TooManyRegisters {
        /// How many registers were asked for.
        count: u16,
        /// The protocol ceiling for this function code.
        max: u16,
    },
    /// The last register asked for sits past address 65 535.
    #[error("a read of {count} registers from {start} runs past the end of the address space")]
    PastEndOfAddressSpace {
        /// Where the read started.
        start: u16,
        /// How many registers were asked for.
        count: u16,
    },
    /// The byte slice is shorter than a Modbus TCP reply needs.
    #[error("the reply is {found} bytes, too few for the {expected} this frame needs")]
    TooShort {
        /// How many bytes this step needed.
        expected: usize,
        /// How many bytes were present.
        found: usize,
    },
    /// The protocol id is not zero, so the frame is not Modbus.
    #[error("the reply carries protocol id {found}, so it is not Modbus")]
    ProtocolId {
        /// The protocol id the frame carried.
        found: u16,
    },
    /// The length field is outside what a Modbus TCP frame can carry.
    #[error("the reply declares {declared} bytes, outside the {min} to {max} a frame can carry")]
    LengthOutOfRange {
        /// The length field from the header.
        declared: u16,
        /// The smallest legal length.
        min: u16,
        /// The largest legal length.
        max: u16,
    },
    /// The length field does not match the bytes that follow it.
    #[error("the reply declares {declared} bytes and carries {found}")]
    LengthMismatch {
        /// The length field from the header.
        declared: u16,
        /// How many bytes followed the header.
        found: u16,
    },
    /// The reply echoed a different transaction id than this read used.
    #[error("the reply answers transaction {found}, and this read is transaction {expected}")]
    TransactionMismatch {
        /// The transaction id this read used.
        expected: u16,
        /// The transaction id the reply echoed.
        found: u16,
    },
    /// The reply came from a different unit than this read addressed.
    #[error("the reply comes from unit {found}, and this read went to unit {expected}")]
    UnitMismatch {
        /// The unit id this read addressed.
        expected: u8,
        /// The unit id the reply named.
        found: u8,
    },
    /// The reply carried a function code this read did not use.
    #[error("the reply carries function code {found}, and this read used {expected}")]
    FunctionMismatch {
        /// The function code this read used.
        expected: u8,
        /// The function code the reply carried.
        found: u8,
    },
    /// The device refused the read and named a reason.
    #[error("the device refused function code {function}: {code}")]
    Exception {
        /// The function code that was refused.
        function: u8,
        /// The reason the device gave.
        code: ExceptionCode,
    },
    /// The reply's byte count is not twice the register count that was asked.
    #[error("the reply carries {declared} bytes of registers, and this read asked for {expected}")]
    ByteCountMismatch {
        /// The byte count the reply declared.
        declared: u16,
        /// The byte count this read is owed.
        expected: u16,
    },
    /// The header length and the PDU byte count do not describe the same body.
    #[error(
        "the reply's header declares {declared} bytes and its byte count implies {byte_count}"
    )]
    InconsistentLength {
        /// The length field from the header.
        declared: u16,
        /// The byte-count field from the PDU.
        byte_count: u8,
    },
    /// A decoder was handed the wrong number of 16-bit words.
    #[error("a {kind} reads {expected} registers and was handed {found}")]
    WordCount {
        /// The decoder's name.
        kind: &'static str,
        /// How many words that decoder needs.
        expected: u16,
        /// How many words were supplied.
        found: usize,
    },
    /// A scale factor must be a finite number.
    #[error("a register scale is a finite number")]
    NonFiniteScale,
    /// A scale of zero would turn every register into nothing.
    #[error("a scale of zero reads every register as nothing")]
    ZeroScale,
}

/// Why a device refused a read, in its own words.
///
/// The specification numbers these. A reply of 2 means the register the
/// caller's map points at is not on this device. "2" does not say that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionCode {
    /// Exception code 1. The function is not supported.
    IllegalFunction,
    /// Exception code 2. The address is not on this device.
    IllegalDataAddress,
    /// Exception code 3. A value in the request is not allowed.
    IllegalDataValue,
    /// Exception code 4. The device failed while handling the request.
    ServerDeviceFailure,
    /// Exception code 5. The device accepted the request and needs time.
    Acknowledge,
    /// Exception code 6. The device is busy.
    ServerDeviceBusy,
    /// Exception code 7. The device refused the request.
    NegativeAcknowledge,
    /// Exception code 8. A memory parity error.
    MemoryParityError,
    /// Exception code 10. A gateway could not path the request.
    GatewayPathUnavailable,
    /// Exception code 11. A gateway got no response from the target.
    GatewayTargetNoResponse,
    /// A code the specification does not name.
    Other(u8),
}

/// The Modbus function codes this crate can express. There are two.
///
/// A two-variant enum is the read-only rule written where the compiler reads
/// it: a write function code is not a value this crate refuses, it is a
/// value nobody can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionCode {
    /// Function code 3. Read holding registers.
    ReadHoldingRegisters,
    /// Function code 4. Read input registers.
    ReadInputRegisters,
}

/// The number a device uses to tell one request from another.
///
/// Any value is legal, and the device echoes it back. Choose a new id for
/// each read. [`ReadRequest::decode`] rejects a late answer to an earlier
/// read by its id, and a constant id makes that check agree with every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransactionId(u16);

/// Which device on the far side of the connection is being asked.
///
/// Zero is a broadcast — a frame nobody answers, which on a read-only client
/// can only be a mistake — and 248 to 254 are reserved. 255 is what the
/// specification tells a device with no serial line behind it to use, so it
/// is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnitId(u8);

/// Where in a device's register map a read starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegisterAddress(u16);

/// How many 16-bit registers a read asks for.
///
/// Function codes 3 and 4 answer with a byte count in a single byte, so 125
/// registers is the protocol's own ceiling and not a limit chosen here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegisterCount(u16);

/// One read, complete enough to be a frame and to judge the frame that comes
/// back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    transaction: TransactionId,
    unit: UnitId,
    function: FunctionCode,
    start: RegisterAddress,
    count: RegisterCount,
}

/// The most registers function codes 3 and 4 can answer in one frame.
pub const MAX_REGISTERS: u16 = 125;

/// The protocol id that says a frame is Modbus. It has one value.
const PROTOCOL_ID: u16 = 0;

/// What an MBAP length counts for a read request: unit id, function code,
/// start address, register count.
const REQUEST_LENGTH: u16 = 6;

/// The bytes ahead of the length field's own subject: transaction id,
/// protocol id, length.
pub(crate) const HEADER_PREFIX_LEN: usize = 6;

/// A request frame: the prefix, the length's own subject, and nothing else.
const REQUEST_FRAME_LEN: usize = HEADER_PREFIX_LEN + REQUEST_LENGTH as usize;

/// The least a reply can declare: a unit id and a function code.
pub(crate) const MIN_LENGTH: u16 = 2;

/// The most a reply can declare: a unit id and the largest PDU the protocol
/// allows.
pub(crate) const MAX_LENGTH: u16 = 254;

/// The shortest frame that can be read as a reply at all.
const MIN_FRAME_LEN: usize = HEADER_PREFIX_LEN + MIN_LENGTH as usize;

/// The bit a device sets on the function code to say it is refusing.
const EXCEPTION_BIT: u8 = 0x80;

/// Where the fields of a reply sit.
const TRANSACTION_AT: usize = 0;
const PROTOCOL_AT: usize = 2;
const LENGTH_AT: usize = 4;
const UNIT_AT: usize = 6;
const FUNCTION_AT: usize = 7;
const PDU_AT: usize = 8;

/// A reply, taken apart but not yet believed.
///
/// Every field is a byte that was there; whether the device answered *this*
/// read is [`ReadRequest::decode`]'s question, not this type's. `pdu` is what
/// follows the function code: a byte count and the registers, or the code of
/// a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reply<'a> {
    declared: u16,
    transaction: u16,
    unit: u8,
    function: u8,
    pdu: &'a [u8],
}

impl Reply<'_> {
    /// This reply, said to be a byte short of what any answer needs.
    fn too_short(&self) -> ModbusError {
        ModbusError::TooShort {
            expected: MIN_FRAME_LEN + 1,
            found: MIN_FRAME_LEN + self.pdu.len(),
        }
    }
}

/// The big-endian pair at `at`, when the frame is long enough to hold one.
fn word_at(frame: &[u8], at: usize) -> Option<u16> {
    let pair: [u8; 2] = frame.get(at..at + 2)?.try_into().ok()?;
    Some(u16::from_be_bytes(pair))
}

/// What a frame must be before anything in it is read as an answer, whoever
/// asked: long enough to hold a header, Modbus, and as long as it says it is.
///
/// # Errors
///
/// Returns [`ModbusError::TooShort`], [`ModbusError::ProtocolId`],
/// [`ModbusError::LengthOutOfRange`] or [`ModbusError::LengthMismatch`].
fn parse_reply(frame: &[u8]) -> Result<Reply<'_>, ModbusError> {
    let (
        Some(transaction),
        Some(protocol),
        Some(declared),
        Some(&unit),
        Some(&function),
        Some(pdu),
    ) = (
        word_at(frame, TRANSACTION_AT),
        word_at(frame, PROTOCOL_AT),
        word_at(frame, LENGTH_AT),
        frame.get(UNIT_AT),
        frame.get(FUNCTION_AT),
        frame.get(PDU_AT..),
    )
    else {
        return Err(ModbusError::TooShort {
            expected: MIN_FRAME_LEN,
            found: frame.len(),
        });
    };
    if protocol != PROTOCOL_ID {
        return Err(ModbusError::ProtocolId { found: protocol });
    }
    if !(MIN_LENGTH..=MAX_LENGTH).contains(&declared) {
        return Err(ModbusError::LengthOutOfRange {
            declared,
            min: MIN_LENGTH,
            max: MAX_LENGTH,
        });
    }
    let carried = u16::try_from(frame.len() - HEADER_PREFIX_LEN).unwrap_or(u16::MAX);
    if carried != declared {
        return Err(ModbusError::LengthMismatch {
            declared,
            found: carried,
        });
    }
    Ok(Reply {
        declared,
        transaction,
        unit,
        function,
        pdu,
    })
}

impl FunctionCode {
    /// The byte this function code puts on the wire.
    ///
    /// Exhaustive by construction: a third variant does not compile until
    /// someone writes here what it would send.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::ReadHoldingRegisters => 3,
            Self::ReadInputRegisters => 4,
        }
    }
}

impl ExceptionCode {
    /// What an operator should read in a log line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::IllegalFunction => "illegal function",
            Self::IllegalDataAddress => "illegal data address",
            Self::IllegalDataValue => "illegal data value",
            Self::ServerDeviceFailure => "server device failure",
            Self::Acknowledge => "acknowledge",
            Self::ServerDeviceBusy => "server device busy",
            Self::NegativeAcknowledge => "negative acknowledge",
            Self::MemoryParityError => "memory parity error",
            Self::GatewayPathUnavailable => "gateway path unavailable",
            Self::GatewayTargetNoResponse => "gateway target device failed to respond",
            Self::Other(_) => "an exception this specification does not define",
        }
    }

    /// The byte a device sends for this refusal.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::IllegalFunction => 1,
            Self::IllegalDataAddress => 2,
            Self::IllegalDataValue => 3,
            Self::ServerDeviceFailure => 4,
            Self::Acknowledge => 5,
            Self::ServerDeviceBusy => 6,
            Self::NegativeAcknowledge => 7,
            Self::MemoryParityError => 8,
            Self::GatewayPathUnavailable => 10,
            Self::GatewayTargetNoResponse => 11,
            Self::Other(byte) => byte,
        }
    }
}

/// Every byte is an exception of some kind: a device that invents one is
/// reported as itself rather than mistaken for a code with a meaning.
impl From<u8> for ExceptionCode {
    fn from(byte: u8) -> Self {
        match byte {
            1 => Self::IllegalFunction,
            2 => Self::IllegalDataAddress,
            3 => Self::IllegalDataValue,
            4 => Self::ServerDeviceFailure,
            5 => Self::Acknowledge,
            6 => Self::ServerDeviceBusy,
            7 => Self::NegativeAcknowledge,
            8 => Self::MemoryParityError,
            10 => Self::GatewayPathUnavailable,
            11 => Self::GatewayTargetNoResponse,
            other => Self::Other(other),
        }
    }
}

impl fmt::Display for ExceptionCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.byte())
    }
}

impl TransactionId {
    /// The id a read will carry, and the id its answer must echo.
    #[must_use]
    pub const fn new(id: u16) -> Self {
        Self(id)
    }

    /// The number itself, for the two bytes it becomes.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl UnitId {
    /// An addressed unit.
    ///
    /// # Errors
    ///
    /// Returns [`ModbusError::BroadcastUnit`] for 0 and
    /// [`ModbusError::ReservedUnit`] for 248 to 254.
    pub const fn new(unit: u8) -> Result<Self, ModbusError> {
        match unit {
            0 => Err(ModbusError::BroadcastUnit),
            248..=254 => Err(ModbusError::ReservedUnit { unit }),
            _ => Ok(Self(unit)),
        }
    }

    /// The byte the frame carries for this unit.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl RegisterAddress {
    /// An address in a device's map, as its register map spells it.
    #[must_use]
    pub const fn new(address: u16) -> Self {
        Self(address)
    }

    /// The address itself, for the two bytes it becomes.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl RegisterCount {
    /// A register count this protocol can answer.
    ///
    /// # Errors
    ///
    /// Returns [`ModbusError::EmptyRead`] for 0 and
    /// [`ModbusError::TooManyRegisters`] above [`MAX_REGISTERS`].
    pub const fn new(count: u16) -> Result<Self, ModbusError> {
        if count == 0 {
            return Err(ModbusError::EmptyRead);
        }
        if count > MAX_REGISTERS {
            return Err(ModbusError::TooManyRegisters {
                count,
                max: MAX_REGISTERS,
            });
        }
        Ok(Self(count))
    }

    /// The count itself, for the two bytes it becomes.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// How many bytes a device owes in answer to this many registers.
    #[must_use]
    pub const fn byte_count(self) -> u16 {
        self.0 * 2
    }
}

impl ReadRequest {
    /// A read a device could answer.
    ///
    /// # Errors
    ///
    /// Returns [`ModbusError::PastEndOfAddressSpace`] when the last register
    /// asked for is past the end of the 16-bit address space — a request no
    /// device can answer, caught where it is written rather than later in an
    /// exception reply.
    pub fn new(
        transaction: TransactionId,
        unit: UnitId,
        function: FunctionCode,
        start: RegisterAddress,
        count: RegisterCount,
    ) -> Result<Self, ModbusError> {
        let last = u32::from(start.get()) + u32::from(count.get());
        if last > u32::from(u16::MAX) + 1 {
            return Err(ModbusError::PastEndOfAddressSpace {
                start: start.get(),
                count: count.get(),
            });
        }
        Ok(Self {
            transaction,
            unit,
            function,
            start,
            count,
        })
    }

    /// The id this read carries, and the id its answer must echo.
    #[must_use]
    pub const fn transaction(&self) -> TransactionId {
        self.transaction
    }

    /// The device this read is addressed to.
    #[must_use]
    pub const fn unit(&self) -> UnitId {
        self.unit
    }

    /// Which of the two read tables this read asks about.
    #[must_use]
    pub const fn function(&self) -> FunctionCode {
        self.function
    }

    /// Where in the map this read starts.
    #[must_use]
    pub const fn start(&self) -> RegisterAddress {
        self.start
    }

    /// How many registers this read asks for, and so how many bytes its
    /// answer owes.
    #[must_use]
    pub const fn count(&self) -> RegisterCount {
        self.count
    }

    /// This read, as the bytes that go on the wire.
    ///
    /// Big-endian throughout, which is the protocol's order and not a choice.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut frame = Vec::with_capacity(REQUEST_FRAME_LEN);
        frame.extend_from_slice(&self.transaction.get().to_be_bytes());
        frame.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
        frame.extend_from_slice(&REQUEST_LENGTH.to_be_bytes());
        frame.push(self.unit.get());
        frame.push(self.function.code());
        frame.extend_from_slice(&self.start.get().to_be_bytes());
        frame.extend_from_slice(&self.count.get().to_be_bytes());
        frame
    }

    /// What a device sent back, as the registers this read asked for.
    ///
    /// The frame is the whole reply, header included, exactly as many bytes
    /// as its length field declares. Everything the device echoed is checked
    /// against this request before a single register is believed, which is
    /// what keeps a late answer to a previous read from being taken as this
    /// one's.
    ///
    /// # Errors
    ///
    /// Returns [`ModbusError::Exception`] when the device refused the read,
    /// and one of the framing or echo variants when the frame is not an
    /// answer to this request. No reply produces a panic and none produces
    /// registers the device did not send.
    pub fn decode(&self, frame: &[u8]) -> Result<Vec<u16>, ModbusError> {
        let reply = parse_reply(frame)?;
        self.check_echo(&reply)?;
        let expected = self.function.code();
        if reply.function == expected {
            return self.registers(&reply);
        }
        if reply.function == expected | EXCEPTION_BIT {
            return Err(exception(expected, &reply));
        }
        Err(ModbusError::FunctionMismatch {
            expected,
            found: reply.function,
        })
    }

    /// That the device answering is the device that was asked, about the read
    /// it was asked.
    fn check_echo(&self, reply: &Reply<'_>) -> Result<(), ModbusError> {
        if reply.transaction != self.transaction.get() {
            return Err(ModbusError::TransactionMismatch {
                expected: self.transaction.get(),
                found: reply.transaction,
            });
        }
        if reply.unit != self.unit.get() {
            return Err(ModbusError::UnitMismatch {
                expected: self.unit.get(),
                found: reply.unit,
            });
        }
        Ok(())
    }

    /// A successful reply's registers, big-endian pairs, in the order the
    /// device sent them.
    fn registers(&self, reply: &Reply<'_>) -> Result<Vec<u16>, ModbusError> {
        let (Some(&byte_count), Some(words)) = (reply.pdu.first(), reply.pdu.get(1..)) else {
            return Err(reply.too_short());
        };
        let expected = self.count.byte_count();
        if u16::from(byte_count) != expected {
            return Err(ModbusError::ByteCountMismatch {
                declared: u16::from(byte_count),
                expected,
            });
        }
        if words.len() != usize::from(byte_count) {
            return Err(ModbusError::InconsistentLength {
                declared: reply.declared,
                byte_count,
            });
        }
        // `chunks_exact(2)` yields slices of exactly two, and the byte count
        // is even, so there is no remainder and neither index can be out of
        // range.
        Ok(words
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect())
    }
}

/// A refusal, as the reason the device gave for it.
fn exception(function: u8, reply: &Reply<'_>) -> ModbusError {
    match reply.pdu.first() {
        Some(&byte) => ModbusError::Exception {
            function,
            code: ExceptionCode::from(byte),
        },
        None => reply.too_short(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExceptionCode, FunctionCode, ModbusError, ReadRequest, RegisterAddress, RegisterCount,
        TransactionId, UnitId, MAX_REGISTERS,
    };

    /// Every named exception code, so a test can walk them all.
    const NAMED_EXCEPTIONS: [ExceptionCode; 10] = [
        ExceptionCode::IllegalFunction,
        ExceptionCode::IllegalDataAddress,
        ExceptionCode::IllegalDataValue,
        ExceptionCode::ServerDeviceFailure,
        ExceptionCode::Acknowledge,
        ExceptionCode::ServerDeviceBusy,
        ExceptionCode::NegativeAcknowledge,
        ExceptionCode::MemoryParityError,
        ExceptionCode::GatewayPathUnavailable,
        ExceptionCode::GatewayTargetNoResponse,
    ];

    /// A frame with the header a test names, however wrong.
    fn framed(transaction: u16, protocol: u16, declared: u16, tail: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.extend_from_slice(&transaction.to_be_bytes());
        frame.extend_from_slice(&protocol.to_be_bytes());
        frame.extend_from_slice(&declared.to_be_bytes());
        frame.extend_from_slice(tail);
        frame
    }

    /// A well-framed reply: Modbus, and as long as it says it is.
    fn reply(transaction: u16, unit: u8, function: u8, pdu: &[u8]) -> Vec<u8> {
        let mut tail = vec![unit, function];
        tail.extend_from_slice(pdu);
        let declared = u16::try_from(tail.len()).expect("a test's reply is short");
        framed(transaction, 0, declared, &tail)
    }

    /// A request built from values a test names, so each test says only what
    /// it is about.
    fn request(
        transaction: u16,
        unit: u8,
        function: FunctionCode,
        start: u16,
        count: u16,
    ) -> ReadRequest {
        ReadRequest::new(
            TransactionId::new(transaction),
            UnitId::new(unit).expect("a test's unit id is addressed"),
            function,
            RegisterAddress::new(start),
            RegisterCount::new(count).expect("a test's register count is within the ceiling"),
        )
        .expect("a test's read stays inside the address space")
    }

    /// Synthetic holding-register read: function code 3, unit 1, address 0,
    /// count 2. The bytes are invented; they are not a capture from a device.
    #[test]
    fn a_holding_register_read_encodes_to_the_named_frame() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);

        let frame = read.encode();

        assert_eq!(
            frame,
            vec![0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01, 0x03, 0x00, 0x00, 0x00, 0x02]
        );
    }

    /// Input registers are the same read of a different table, so the frames
    /// differ in the function code and nowhere else.
    #[test]
    fn an_input_register_read_differs_from_a_holding_read_in_one_byte() {
        let holding = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2).encode();
        let input = request(1, 1, FunctionCode::ReadInputRegisters, 0, 2).encode();

        let differing: Vec<usize> = holding
            .iter()
            .zip(input.iter())
            .enumerate()
            .filter(|(_, (left, right))| left != right)
            .map(|(index, _)| index)
            .collect();

        assert_eq!(differing, vec![7]);
        assert_eq!((holding[7], input[7]), (3, 4));
    }

    #[test]
    fn a_read_high_in_the_map_encodes_its_transaction_address_and_count() {
        let read = request(0x1234, 17, FunctionCode::ReadInputRegisters, 0x0100, 125);

        let frame = read.encode();

        assert_eq!(
            frame,
            vec![0x12, 0x34, 0x00, 0x00, 0x00, 0x06, 0x11, 0x04, 0x01, 0x00, 0x00, 0x7d]
        );
    }

    #[test]
    fn the_two_function_codes_are_three_and_four() {
        assert_eq!(FunctionCode::ReadHoldingRegisters.code(), 3);
        assert_eq!(FunctionCode::ReadInputRegisters.code(), 4);
    }

    #[test]
    fn a_broadcast_unit_is_refused() {
        let refused = UnitId::new(0);

        assert_eq!(refused, Err(ModbusError::BroadcastUnit));
    }

    #[test]
    fn a_reserved_unit_is_refused_by_number() {
        let refused = UnitId::new(250);

        assert_eq!(refused, Err(ModbusError::ReservedUnit { unit: 250 }));
    }

    /// 255 is the specification's own value for a device that has no serial
    /// line behind it, and 247 is the top of the addressable range.
    #[test]
    fn an_addressed_unit_is_accepted_including_the_edges() {
        for unit in [1, 247, 255] {
            assert_eq!(UnitId::new(unit).map(UnitId::get), Ok(unit));
        }
    }

    #[test]
    fn a_read_of_no_registers_is_refused() {
        let refused = RegisterCount::new(0);

        assert_eq!(refused, Err(ModbusError::EmptyRead));
    }

    #[test]
    fn a_read_above_the_protocols_ceiling_is_refused_naming_it() {
        let refused = RegisterCount::new(126);

        assert_eq!(
            refused,
            Err(ModbusError::TooManyRegisters {
                count: 126,
                max: MAX_REGISTERS,
            })
        );
    }

    #[test]
    fn a_read_at_the_ceiling_is_accepted() {
        let count = RegisterCount::new(MAX_REGISTERS);

        assert_eq!(count.map(RegisterCount::get), Ok(MAX_REGISTERS));
    }

    #[test]
    fn a_register_count_says_how_many_bytes_it_is_owed() {
        let count = RegisterCount::new(2).expect("two registers");

        assert_eq!(count.byte_count(), 4);
    }

    #[test]
    fn a_read_running_past_the_end_of_the_address_space_is_refused() {
        let refused = ReadRequest::new(
            TransactionId::new(1),
            UnitId::new(1).expect("an addressed unit"),
            FunctionCode::ReadHoldingRegisters,
            RegisterAddress::new(u16::MAX),
            RegisterCount::new(2).expect("two registers"),
        );

        assert_eq!(
            refused,
            Err(ModbusError::PastEndOfAddressSpace {
                start: u16::MAX,
                count: 2,
            })
        );
    }

    /// The last two registers in the map are a read, not an overrun: the
    /// boundary is the end of the space, not the last address.
    #[test]
    fn a_read_ending_on_the_last_register_is_accepted() {
        let read = ReadRequest::new(
            TransactionId::new(1),
            UnitId::new(1).expect("an addressed unit"),
            FunctionCode::ReadHoldingRegisters,
            RegisterAddress::new(u16::MAX - 1),
            RegisterCount::new(2).expect("two registers"),
        );

        assert!(read.is_ok());
    }

    #[test]
    fn a_normal_reply_decodes_to_its_register_words() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = reply(1, 1, 3, &[4, 0x00, 0x2a, 0x01, 0x90]);

        let registers = read.decode(&frame);

        assert_eq!(registers, Ok(vec![42, 400]));
    }

    /// Synthetic exception reply: unit 1, function code 3 refused, illegal
    /// data address. Invented bytes; not a capture from a device.
    #[test]
    fn the_named_exception_reply_is_an_illegal_data_address() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = reply(1, 1, 0x83, &[2]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::Exception {
                function: 3,
                code: ExceptionCode::IllegalDataAddress,
            })
        );
        assert!(refused
            .unwrap_err()
            .to_string()
            .contains("illegal data address"));
    }

    #[test]
    fn an_input_register_read_reports_its_own_exception() {
        let read = request(1, 1, FunctionCode::ReadInputRegisters, 0, 2);
        let frame = reply(1, 1, 0x84, &[11]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::Exception {
                function: 4,
                code: ExceptionCode::GatewayTargetNoResponse,
            })
        );
    }

    #[test]
    fn an_exception_code_the_specification_does_not_define_is_reported_as_itself() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = reply(1, 1, 0x83, &[99]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::Exception {
                function: 3,
                code: ExceptionCode::Other(99),
            })
        );
    }

    #[test]
    fn every_named_exception_code_round_trips_through_its_byte() {
        for code in NAMED_EXCEPTIONS {
            assert_eq!(ExceptionCode::from(code.byte()), code);
            assert!(!code.name().is_empty());
        }

        assert_eq!(ExceptionCode::Other(99).byte(), 99);
    }

    #[test]
    fn an_empty_frame_is_too_short() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);

        let refused = read.decode(&[]);

        assert_eq!(
            refused,
            Err(ModbusError::TooShort {
                expected: 8,
                found: 0,
            })
        );
    }

    #[test]
    fn a_header_with_no_body_is_too_short() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = framed(1, 0, 2, &[]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::TooShort {
                expected: 8,
                found: 6,
            })
        );
    }

    #[test]
    fn a_frame_that_is_not_modbus_is_refused_by_its_protocol_id() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = framed(1, 1, 2, &[1, 3]);

        let refused = read.decode(&frame);

        assert_eq!(refused, Err(ModbusError::ProtocolId { found: 1 }));
    }

    #[test]
    fn a_declared_length_outside_what_a_frame_can_carry_is_refused() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let too_small = framed(1, 0, 1, &[1, 3]);
        let too_large = framed(1, 0, 255, &[1, 3]);

        let refusals = [read.decode(&too_small), read.decode(&too_large)];

        assert_eq!(
            refusals,
            [
                Err(ModbusError::LengthOutOfRange {
                    declared: 1,
                    min: 2,
                    max: 254,
                }),
                Err(ModbusError::LengthOutOfRange {
                    declared: 255,
                    min: 2,
                    max: 254,
                }),
            ]
        );
    }

    /// A frame cut short in transit declares more than it carries. Decoding
    /// what arrived would report registers the device never sent.
    #[test]
    fn a_reply_that_declares_more_than_it_carries_is_refused() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = framed(1, 0, 7, &[1, 3, 4, 0x00, 0x2a]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::LengthMismatch {
                declared: 7,
                found: 5,
            })
        );
    }

    /// The frame a device left behind after the caller gave up on it: whole,
    /// well-formed, and an answer to the previous read.
    #[test]
    fn a_reply_to_another_transaction_is_refused_rather_than_decoded() {
        let read = request(2, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let stale = reply(1, 1, 3, &[4, 0x00, 0x2a, 0x01, 0x90]);

        let refused = read.decode(&stale);

        assert_eq!(
            refused,
            Err(ModbusError::TransactionMismatch {
                expected: 2,
                found: 1,
            })
        );
    }

    #[test]
    fn a_reply_from_another_unit_is_refused() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = reply(1, 9, 3, &[4, 0x00, 0x2a, 0x01, 0x90]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::UnitMismatch {
                expected: 1,
                found: 9,
            })
        );
    }

    #[test]
    fn a_reply_carrying_another_function_code_is_refused() {
        let read = request(1, 1, FunctionCode::ReadInputRegisters, 0, 2);
        let frame = reply(1, 1, 3, &[4, 0x00, 0x2a, 0x01, 0x90]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::FunctionMismatch {
                expected: 4,
                found: 3,
            })
        );
    }

    #[test]
    fn a_reply_with_fewer_registers_than_were_asked_for_is_refused() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = reply(1, 1, 3, &[2, 0x00, 0x2a]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::ByteCountMismatch {
                declared: 2,
                expected: 4,
            })
        );
    }

    #[test]
    fn a_reply_whose_header_and_byte_count_disagree_is_refused() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = reply(1, 1, 3, &[4, 0x00, 0x2a, 0x01, 0x90, 0x00]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::InconsistentLength {
                declared: 8,
                byte_count: 4,
            })
        );
    }

    #[test]
    fn a_reply_with_no_byte_count_is_too_short() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = reply(1, 1, 3, &[]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::TooShort {
                expected: 9,
                found: 8,
            })
        );
    }

    #[test]
    fn an_exception_reply_with_no_code_is_too_short() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let frame = reply(1, 1, 0x83, &[]);

        let refused = read.decode(&frame);

        assert_eq!(
            refused,
            Err(ModbusError::TooShort {
                expected: 9,
                found: 8,
            })
        );
    }

    /// The whole ceiling, decoded: 125 registers is the largest answer this
    /// protocol can give, and it arrives as 125 words.
    #[test]
    fn a_reply_at_the_register_ceiling_decodes_to_every_word() {
        let read = request(1, 1, FunctionCode::ReadInputRegisters, 0, MAX_REGISTERS);
        let mut pdu = vec![250];
        for word in 0..MAX_REGISTERS {
            pdu.extend_from_slice(&word.to_be_bytes());
        }
        let frame = reply(1, 1, 4, &pdu);

        let registers = read.decode(&frame);

        assert_eq!(registers, Ok((0..MAX_REGISTERS).collect::<Vec<u16>>()));
    }

    #[test]
    fn a_request_keeps_the_values_it_was_built_from() {
        let read = request(7, 9, FunctionCode::ReadInputRegisters, 100, 4);

        assert_eq!(read.transaction().get(), 7);
        assert_eq!(read.unit().get(), 9);
        assert_eq!(read.function(), FunctionCode::ReadInputRegisters);
        assert_eq!(read.start().get(), 100);
        assert_eq!(read.count().get(), 4);
    }
}
