//! A Modbus TCP device made of `std`, for tests and examples.
//!
//! It listens on `127.0.0.1` at an OS-assigned port, serves fixed synthetic
//! registers to function codes 3 and 4, and answers anything else with an
//! exception. The values are invented. They are not readings from a real
//! device.
//!
//! | Function code | Address | Registers | Read as |
//! |---|---|---|---|
//! | 3 | 0 | `4248 0000` | `f32be` = 50.0 |
//! | 4 | 10 | `ff38` | `i16` = −200 |
//!
//! A write function code has to be written by hand to reach this server,
//! because [`crate::FunctionCode`] cannot name one.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

/// The unit id the device answers as.
const UNIT: u8 = 1;

/// Holding-register start of the synthetic float pair.
const HOLDING_AT: u16 = 0;

/// Input-register address of the synthetic signed word.
const INPUT_AT: u16 = 10;

/// The refusal the device gives a function code it does not serve.
const ILLEGAL_FUNCTION: u8 = 1;

/// A loopback Modbus TCP server that serves fixed synthetic registers.
///
/// Drop the server to stop accepting connections. The register values are
/// invented for tests. They are not readings from a real device.
pub struct TestServer {
    addr: SocketAddr,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestServer {
    /// Listen on `127.0.0.1` at an OS-assigned port.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when loopback cannot be bound.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        let thread = thread::spawn(move || {
            while flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if !flag.load(Ordering::Relaxed) {
                            break;
                        }
                        serve(stream);
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            addr,
            running,
            thread: Some(thread),
        })
    }

    /// The loopback address this server is listening on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Unblock `accept` so the thread can exit.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answer every frame on one connection until the client goes away.
fn serve(mut stream: TcpStream) {
    let mut header = [0_u8; 6];
    while stream.read_exact(&mut header).is_ok() {
        let declared = usize::from(u16::from_be_bytes([header[4], header[5]]));
        let mut body = vec![0_u8; declared];
        if stream.read_exact(&mut body).is_err() {
            return;
        }
        let transaction = u16::from_be_bytes([header[0], header[1]]);
        let reply = answer(transaction, &body);
        if stream.write_all(&reply).is_err() {
            return;
        }
    }
}

/// What this device says to one request body (unit, function, and the rest).
fn answer(transaction: u16, body: &[u8]) -> Vec<u8> {
    let (Some(&unit), Some(&function), Some(address)) = (
        body.first(),
        body.get(1),
        body.get(2..4)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]])),
    ) else {
        return exception(transaction, UNIT, 0, ILLEGAL_FUNCTION);
    };
    match (function, address) {
        (3, HOLDING_AT) => registers(transaction, unit, function, &[0x4248, 0x0000]),
        (4, INPUT_AT) => registers(transaction, unit, function, &[0xFF38]),
        // Every other function code, write function code 0x06 included: this
        // device serves reads and refuses the rest.
        _ => exception(transaction, unit, function, ILLEGAL_FUNCTION),
    }
}

/// A normal reply carrying `words`.
fn registers(transaction: u16, unit: u8, function: u8, words: &[u16]) -> Vec<u8> {
    let byte_count = u8::try_from(words.len() * 2).expect("this device serves short reads");
    let mut pdu = vec![unit, function, byte_count];
    for word in words {
        pdu.extend_from_slice(&word.to_be_bytes());
    }
    frame(transaction, &pdu)
}

/// A refusal, as the specification spells one: the function code with its top
/// bit set, then the reason.
fn exception(transaction: u16, unit: u8, function: u8, code: u8) -> Vec<u8> {
    frame(transaction, &[unit, function | 0x80, code])
}

/// An MBAP header in front of a PDU.
fn frame(transaction: u16, pdu: &[u8]) -> Vec<u8> {
    let length = u16::try_from(pdu.len()).expect("this device serves short frames");
    let mut out = Vec::with_capacity(6 + pdu.len());
    out.extend_from_slice(&transaction.to_be_bytes());
    out.extend_from_slice(&0_u16.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(pdu);
    out
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::{TestServer, HOLDING_AT, ILLEGAL_FUNCTION, INPUT_AT, UNIT};
    use crate::{
        read_registers, ExceptionCode, ExchangeError, FunctionCode, ModbusError, ReadRequest,
        RegisterAddress, RegisterCount, RegisterKind, Scale, TransactionId, UnitId,
    };

    fn request(function: FunctionCode, start: u16, count: u16) -> ReadRequest {
        ReadRequest::new(
            TransactionId::new(1),
            UnitId::new(UNIT).expect("unit 1 is addressed"),
            function,
            RegisterAddress::new(start),
            RegisterCount::new(count).expect("a test count"),
        )
        .expect("a read inside the address space")
    }

    /// The two synthetic quantities, over a real socket.
    #[test]
    fn a_client_reads_the_synthetic_registers_over_loopback() {
        let server = TestServer::start().expect("loopback is available");
        let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");

        let holding = request(FunctionCode::ReadHoldingRegisters, HOLDING_AT, 2);
        let words = read_registers(&mut stream, &holding).expect("holding registers");
        let value = RegisterKind::F32Be.decode(&words).expect("two words");
        assert!((Scale::one().apply(value) - 50.0).abs() < 1e-9);

        let input = request(FunctionCode::ReadInputRegisters, INPUT_AT, 1);
        let words = read_registers(&mut stream, &input).expect("input register");
        let value = RegisterKind::I16.decode(&words).expect("one word");
        let scaled = Scale::new(0.1).expect("a tenth").apply(value);
        assert!((scaled - -20.0).abs() < 1e-9);
    }

    #[test]
    fn the_server_reports_its_loopback_address() {
        let server = TestServer::start().expect("loopback is available");

        assert_eq!(server.addr().ip().to_string(), "127.0.0.1");
        assert_ne!(server.addr().port(), 0);
    }

    /// A register this device does not serve is a refusal it names, not a number.
    #[test]
    fn an_address_the_device_does_not_serve_is_a_refusal() {
        let server = TestServer::start().expect("loopback is available");
        let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");
        let elsewhere = request(FunctionCode::ReadHoldingRegisters, 999, 2);

        let refused = read_registers(&mut stream, &elsewhere);

        assert!(matches!(
            refused,
            Err(ExchangeError::Modbus(ModbusError::Exception {
                function: 3,
                code: ExceptionCode::IllegalFunction,
            }))
        ));
    }

    /// The device refuses a write, and the frame has to be written by hand to
    /// ask it: [`FunctionCode`] has two variants and neither is 6, so no
    /// client in this crate can send this.
    #[test]
    fn the_device_refuses_a_function_code_that_writes() {
        let server = TestServer::start().expect("loopback is available");
        let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");
        // Transaction 1, unit 1, function code 0x06, register 0, value 1.
        // Invented bytes; the client cannot name this function code.
        let write_by_hand: [u8; 12] = [0, 1, 0, 0, 0, 6, UNIT, 0x06, 0, 0, 0, 1];

        stream
            .write_all(&write_by_hand)
            .expect("the request is written");
        let mut reply = [0_u8; 9];
        stream.read_exact(&mut reply).expect("the device answers");

        assert_eq!(reply, [0, 1, 0, 0, 0, 3, UNIT, 0x86, ILLEGAL_FUNCTION]);
    }
}
