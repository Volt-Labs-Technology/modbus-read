//! Read-only Modbus TCP client.
//!
//! Modbus TCP is a request/response protocol: an MBAP header sits in front
//! of a short PDU. This crate encodes a read, decodes the reply, and turns
//! the 16-bit register words into numbers.
//!
//! **A write cannot be spelled.** [`FunctionCode`] has two variants, 3 and 4.
//! A write function code is not a value this crate refuses at run time; it is
//! a value that does not exist.
//!
//! **The request decodes its own answer.** A device that answered a read the
//! caller had given up on leaves a whole, well-formed frame behind it. With
//! the request in hand, the echoed transaction id, unit id, function code and
//! register count are all checked, and that frame is a typed refusal instead.
//! Vary the [`TransactionId`] between reads; a constant id makes the echo
//! check agree with every frame.
//!
//! **One action, and it owns no connection.** [`read_registers`] writes a
//! frame and reads the frame that comes back over anything that is
//! [`std::io::Read`] and [`std::io::Write`]. It holds no socket.
//!
//! Enable the `test-server` feature for a loopback device that serves
//! synthetic registers and refuses every other function code.

#![deny(missing_docs)]

mod decode;
mod frame;

#[cfg(feature = "test-server")]
mod server;

use std::io::{Read, Write};

pub use decode::{RegisterKind, Scale};
pub use frame::{
    ExceptionCode, FunctionCode, ModbusError, ReadRequest, RegisterAddress, RegisterCount,
    TransactionId, UnitId, MAX_REGISTERS,
};

#[cfg(feature = "test-server")]
pub use server::TestServer;

use frame::{HEADER_PREFIX_LEN, MAX_LENGTH, MIN_LENGTH};

/// What an exchange refuses: the frame, or the transport under it.
///
/// Two enums rather than one, because there are two kinds of code here. The
/// calculations cannot fail for a reason the operating system invented, and
/// keeping the I/O variant out of [`ModbusError`] is what says so.
#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    /// The bytes were not an answer to this read.
    #[error(transparent)]
    Modbus(#[from] ModbusError),
    /// The transport failed or ended before a whole frame arrived.
    #[error("the transport failed during a register read: {0}")]
    Io(#[from] std::io::Error),
}

/// Ask a device for the registers a read names, and read its answer.
///
/// The only action in this crate's default feature set. It writes the
/// request's own bytes, reads exactly the header, takes the body's length
/// from it, reads exactly that many more, and hands the whole frame to
/// [`ReadRequest::decode`]. Reading by the declared length is what makes
/// this correct over a stream, where a message boundary is not something the
/// transport preserves — two replies arriving together are two frames, not
/// one long one.
///
/// A device that declares a body no Modbus frame can carry is refused before
/// a byte of it is read, so a wrong length cannot make this wait for bytes
/// that are not coming. How long to wait for the bytes that are is the
/// connection's business.
///
/// # Errors
///
/// Returns [`ExchangeError::Io`] when the transport fails or ends early, and
/// [`ExchangeError::Modbus`] when what arrived is not an answer to this
/// request.
pub fn read_registers<T: Read + Write>(
    transport: &mut T,
    request: &ReadRequest,
) -> Result<Vec<u16>, ExchangeError> {
    transport.write_all(&request.encode())?;
    transport.flush()?;

    let mut header = [0_u8; HEADER_PREFIX_LEN];
    transport.read_exact(&mut header)?;
    let [_, _, _, _, length_high, length_low] = header;
    let declared = u16::from_be_bytes([length_high, length_low]);
    if !(MIN_LENGTH..=MAX_LENGTH).contains(&declared) {
        return Err(ModbusError::LengthOutOfRange {
            declared,
            min: MIN_LENGTH,
            max: MAX_LENGTH,
        }
        .into());
    }

    let mut frame = vec![0_u8; HEADER_PREFIX_LEN + usize::from(declared)];
    let (prefix, body) = frame.split_at_mut(HEADER_PREFIX_LEN);
    prefix.copy_from_slice(&header);
    transport.read_exact(body)?;
    Ok(request.decode(&frame)?)
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};

    use super::{
        read_registers, ExceptionCode, ExchangeError, FunctionCode, ModbusError, ReadRequest,
        RegisterAddress, RegisterCount, TransactionId, UnitId,
    };

    /// A well-framed reply: Modbus, and as long as it says it is.
    fn reply(transaction: u16, unit: u8, function: u8, pdu: &[u8]) -> Vec<u8> {
        let mut tail = vec![unit, function];
        tail.extend_from_slice(pdu);
        let declared = u16::try_from(tail.len()).expect("a test's reply is short");
        let mut frame = Vec::new();
        frame.extend_from_slice(&transaction.to_be_bytes());
        frame.extend_from_slice(&0_u16.to_be_bytes());
        frame.extend_from_slice(&declared.to_be_bytes());
        frame.extend_from_slice(&tail);
        frame
    }

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

    /// A device made of bytes: it keeps what was written to it and answers
    /// from what a test queued. The bytes are synthetic.
    #[derive(Debug)]
    struct Loopback {
        written: Vec<u8>,
        answer: Cursor<Vec<u8>>,
    }

    /// A transport that cannot be written to at all.
    #[derive(Debug)]
    struct Deaf;

    impl Loopback {
        fn answering(answer: Vec<u8>) -> Self {
            Self {
                written: Vec::new(),
                answer: Cursor::new(answer),
            }
        }
    }

    impl Read for Loopback {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.answer.read(buf)
        }
    }

    impl Write for Loopback {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Read for Deaf {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for Deaf {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_exchange_writes_the_request_and_returns_what_the_device_answered() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let mut device = Loopback::answering(reply(1, 1, 3, &[4, 0x00, 0x2a, 0x01, 0x90]));

        let registers = read_registers(&mut device, &read).expect("the device answered");

        assert_eq!(registers, vec![42, 400]);
        assert_eq!(device.written, read.encode());
    }

    /// The frame after the frame: a stream keeps no message boundary, so the
    /// read stops at the length the header declared and leaves the rest where
    /// it is.
    #[test]
    fn an_exchange_reads_one_frame_and_leaves_what_follows_it() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let mut answer = reply(1, 1, 3, &[4, 0x00, 0x2a, 0x01, 0x90]);
        let trailing = reply(2, 1, 3, &[4, 0x00, 0x01, 0x00, 0x02]);
        answer.extend_from_slice(&trailing);
        let mut device = Loopback::answering(answer);

        let registers = read_registers(&mut device, &read).expect("the device answered");

        assert_eq!(registers, vec![42, 400]);
        let mut left = Vec::new();
        device
            .read_to_end(&mut left)
            .expect("the rest is still there");
        assert_eq!(left, trailing);
    }

    #[test]
    fn a_device_that_stops_mid_frame_is_a_transport_failure() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let mut cut = reply(1, 1, 3, &[4, 0x00, 0x2a, 0x01, 0x90]);
        cut.truncate(9);
        let mut device = Loopback::answering(cut);

        let refused = read_registers(&mut device, &read);

        assert!(matches!(refused, Err(ExchangeError::Io(_))));
    }

    #[test]
    fn a_transport_that_refuses_the_write_is_a_transport_failure() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);

        let refused = read_registers(&mut Deaf, &read);

        assert!(matches!(refused, Err(ExchangeError::Io(_))));
    }

    #[test]
    fn a_devices_refusal_arrives_through_the_exchange_as_a_modbus_error() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let mut device = Loopback::answering(reply(1, 1, 0x83, &[2]));

        let refused = read_registers(&mut device, &read);

        assert!(matches!(
            refused,
            Err(ExchangeError::Modbus(ModbusError::Exception {
                code: ExceptionCode::IllegalDataAddress,
                ..
            }))
        ));
    }

    /// A body no frame can carry is refused on the header alone, so a wrong
    /// length never makes the read wait for bytes that are not coming.
    #[test]
    fn a_body_longer_than_any_frame_is_refused_before_it_is_read() {
        let read = request(1, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let mut header = Vec::new();
        header.extend_from_slice(&1_u16.to_be_bytes());
        header.extend_from_slice(&0_u16.to_be_bytes());
        header.extend_from_slice(&u16::MAX.to_be_bytes());
        let mut device = Loopback::answering(header);

        let refused = read_registers(&mut device, &read);

        assert!(matches!(
            refused,
            Err(ExchangeError::Modbus(ModbusError::LengthOutOfRange {
                declared: u16::MAX,
                ..
            }))
        ));
    }

    #[test]
    fn a_stale_frame_does_not_become_this_reads_answer() {
        let read = request(2, 1, FunctionCode::ReadHoldingRegisters, 0, 2);
        let mut device = Loopback::answering(reply(1, 1, 3, &[4, 0x00, 0x2a, 0x01, 0x90]));

        let refused = read_registers(&mut device, &read);

        assert!(matches!(
            refused,
            Err(ExchangeError::Modbus(ModbusError::TransactionMismatch {
                expected: 2,
                found: 1,
            }))
        ));
    }
}
