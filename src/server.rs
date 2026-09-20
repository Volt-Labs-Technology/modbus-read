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
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

use crate::frame::{MAX_LENGTH, MIN_LENGTH};

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
/// Drop the server to close live connections and stop accepting. The register
/// values are invented for tests. They are not readings from a real device.
pub struct TestServer {
    addr: SocketAddr,
    live: Arc<Mutex<Live>>,
    thread: Option<JoinHandle<()>>,
}

/// Running flag and sockets Drop shuts, one lock so a clone cannot appear
/// after Drop has taken the list.
struct Live {
    running: bool,
    connections: Vec<TcpStream>,
}

/// Why an accepted stream must not be served: Drop has no handle that can
/// unblock `read_exact`.
#[derive(Debug)]
enum RememberError {
    Clone,
    Stopped,
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
        let live = Arc::new(Mutex::new(Live {
            running: true,
            connections: Vec::new(),
        }));
        let for_thread = Arc::clone(&live);
        let thread = thread::spawn(move || accept_loop(&listener, &for_thread));
        Ok(Self {
            addr,
            live,
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
        shut_down_live(&self.live);
        // Unblock `accept` so the thread can exit.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept_loop(listener: &TcpListener, live: &Mutex<Live>) {
    loop {
        let Ok((stream, _)) = listener.accept() else {
            break;
        };
        match remember(live, &stream) {
            Ok(()) => {
                serve(stream);
                release(live);
            }
            Err(RememberError::Stopped) => {
                let _ = stream.shutdown(Shutdown::Both);
                break;
            }
            Err(RememberError::Clone) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }
}

fn lock(live: &Mutex<Live>) -> MutexGuard<'_, Live> {
    // A poisoned lock still holds the sockets Drop must shut.
    live.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A cloned handle is how Drop unblocks `read_exact` without a read timeout.
/// Failure means this stream is not in that list, so it must not be served.
fn remember(live: &Mutex<Live>, stream: &TcpStream) -> Result<(), RememberError> {
    let Ok(clone) = stream.try_clone() else {
        return Err(RememberError::Clone);
    };
    let mut guard = lock(live);
    if !guard.running {
        return Err(RememberError::Stopped);
    }
    guard.connections.push(clone);
    Ok(())
}

fn release(live: &Mutex<Live>) {
    let stream = lock(live).connections.pop();
    if let Some(stream) = stream {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

fn shut_down_live(live: &Mutex<Live>) {
    let connections = {
        let mut guard = lock(live);
        guard.running = false;
        std::mem::take(&mut guard.connections)
    };
    for stream in connections {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

/// The frame layer's length law, applied here so an illegal length never
/// becomes an allocated body.
fn is_frame_length(declared: u16) -> bool {
    (MIN_LENGTH..=MAX_LENGTH).contains(&declared)
}

/// Answer every frame on one connection until the client goes away.
fn serve(mut stream: TcpStream) {
    let mut header = [0_u8; 6];
    while stream.read_exact(&mut header).is_ok() {
        let declared = u16::from_be_bytes([header[4], header[5]]);
        if !is_frame_length(declared) {
            return;
        }
        let mut body = vec![0_u8; usize::from(declared)];
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
        return exception(transaction, UNIT, 0, ILLEGAL_FUNCTION).to_vec();
    };
    match (function, address) {
        (3, HOLDING_AT) => holding_reply(transaction, unit, function, [0x4248, 0x0000]).to_vec(),
        (4, INPUT_AT) => input_reply(transaction, unit, function, [0xFF38]).to_vec(),
        // Every other function code, write function code 0x06 included: this
        // device serves reads and refuses the rest.
        _ => exception(transaction, unit, function, ILLEGAL_FUNCTION).to_vec(),
    }
}

/// Two holding words as a Modbus TCP reply. The width is the type: this
/// device does not serve a longer holding read.
fn holding_reply(transaction: u16, unit: u8, function: u8, words: [u16; 2]) -> [u8; 13] {
    let [t0, t1] = transaction.to_be_bytes();
    let [b0, b1] = words[0].to_be_bytes();
    let [b2, b3] = words[1].to_be_bytes();
    [t0, t1, 0, 0, 0, 7, unit, function, 4, b0, b1, b2, b3]
}

/// One input word as a Modbus TCP reply. The width is the type: this device
/// does not serve a longer input read.
fn input_reply(transaction: u16, unit: u8, function: u8, words: [u16; 1]) -> [u8; 11] {
    let [t0, t1] = transaction.to_be_bytes();
    let [hi, lo] = words[0].to_be_bytes();
    [t0, t1, 0, 0, 0, 5, unit, function, 2, hi, lo]
}

/// A refusal, as the specification spells one: the function code with its top
/// bit set, then the reason. The PDU is three bytes, so the length cannot miss.
fn exception(transaction: u16, unit: u8, function: u8, code: u8) -> [u8; 9] {
    let [t0, t1] = transaction.to_be_bytes();
    [t0, t1, 0, 0, 0, 3, unit, function | 0x80, code]
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpStream;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        exception, holding_reply, input_reply, is_frame_length, TestServer, HOLDING_AT,
        ILLEGAL_FUNCTION, INPUT_AT, UNIT,
    };
    use crate::frame::{MAX_LENGTH, MIN_LENGTH};
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

    #[test]
    fn two_holding_words_encode_as_function_code_three() {
        let frame = holding_reply(1, UNIT, 3, [0x4248, 0x0000]);

        assert_eq!(
            frame,
            [0, 1, 0, 0, 0, 7, UNIT, 3, 4, 0x42, 0x48, 0x00, 0x00]
        );
    }

    #[test]
    fn one_input_word_encodes_as_function_code_four() {
        let frame = input_reply(1, UNIT, 4, [0xFF38]);

        assert_eq!(frame, [0, 1, 0, 0, 0, 5, UNIT, 4, 2, 0xFF, 0x38]);
    }

    #[test]
    fn an_exception_is_a_three_byte_pdu() {
        let frame = exception(1, UNIT, 3, ILLEGAL_FUNCTION);

        assert_eq!(frame, [0, 1, 0, 0, 0, 3, UNIT, 0x83, ILLEGAL_FUNCTION]);
    }

    #[test]
    fn a_frame_length_is_the_range_the_frame_layer_names() {
        assert!(is_frame_length(MIN_LENGTH));
        assert!(is_frame_length(MAX_LENGTH));
        assert!(!is_frame_length(0));
        assert!(!is_frame_length(MIN_LENGTH - 1));
        assert!(!is_frame_length(MAX_LENGTH + 1));
        assert!(!is_frame_length(u16::MAX));
    }

    /// Transaction 1, protocol 0, a declared length no frame can carry.
    /// Invented bytes; not a capture from a device.
    fn header_declaring(length: u16) -> [u8; 6] {
        let [hi, lo] = length.to_be_bytes();
        [0, 1, 0, 0, hi, lo]
    }

    fn the_connection_closed_without_an_answer(result: std::io::Result<usize>) {
        match result {
            Ok(0) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::BrokenPipe
                        | ErrorKind::UnexpectedEof
                ) => {}
            other => panic!("illegal length must close, not answer or hang: {other:?}"),
        }
    }

    #[test]
    fn an_mbap_length_below_two_closes_without_an_exception() {
        let server = TestServer::start().expect("loopback is available");
        let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("a test can time out a read");
        let header = header_declaring(0);

        stream.write_all(&header).expect("the header is written");
        let mut buf = [0_u8; 9];
        let closed = stream.read(&mut buf);

        the_connection_closed_without_an_answer(closed);
    }

    #[test]
    fn an_mbap_length_of_one_closes_without_an_exception() {
        let server = TestServer::start().expect("loopback is available");
        let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("a test can time out a read");
        let header = header_declaring(1);

        stream.write_all(&header).expect("the header is written");
        let mut buf = [0_u8; 9];
        let closed = stream.read(&mut buf);

        the_connection_closed_without_an_answer(closed);
    }

    #[test]
    fn an_mbap_length_of_255_closes_without_waiting_for_a_body() {
        let server = TestServer::start().expect("loopback is available");
        let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("a test can time out a read");
        let header = header_declaring(255);

        stream.write_all(&header).expect("the header is written");
        let mut buf = [0_u8; 9];
        let closed = stream.read(&mut buf);

        the_connection_closed_without_an_answer(closed);
    }

    #[test]
    fn an_mbap_length_of_65535_closes_without_waiting_for_a_body() {
        let server = TestServer::start().expect("loopback is available");
        let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("a test can time out a read");
        let header = header_declaring(u16::MAX);

        stream.write_all(&header).expect("the header is written");
        let mut buf = [0_u8; 9];
        let closed = stream.read(&mut buf);

        the_connection_closed_without_an_answer(closed);
    }

    fn assert_holding_float_is_fifty(stream: &mut TcpStream) {
        let holding = request(FunctionCode::ReadHoldingRegisters, HOLDING_AT, 2);
        let words = read_registers(stream, &holding).expect("holding registers");
        let value = RegisterKind::F32Be.decode(&words).expect("two words");
        assert!((Scale::one().apply(value) - 50.0).abs() < 1e-9);
    }

    fn the_peer_closed_after_drop(result: std::io::Result<usize>) {
        match result {
            Ok(0) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::BrokenPipe
                        | ErrorKind::UnexpectedEof
                ) => {}
            other => panic!("drop must close the live client, not answer or hang: {other:?}"),
        }
    }

    #[test]
    fn dropping_the_server_while_a_client_is_connected_returns() {
        let server = TestServer::start().expect("loopback is available");
        let addr = server.addr();
        let mut stream = TcpStream::connect(addr).expect("the server accepts");
        assert_holding_float_is_fifty(&mut stream);

        let (done, rx) = mpsc::channel();
        let started = Instant::now();
        thread::spawn(move || {
            drop(server);
            let _ = done.send(());
        });

        rx.recv_timeout(Duration::from_secs(2))
            .expect("dropping the test server hung while a client was connected");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "drop waited on the client: {:?}",
            started.elapsed()
        );

        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("a test can time out a read");
        let mut buf = [0_u8; 9];
        the_peer_closed_after_drop(stream.read(&mut buf));

        assert!(
            TcpStream::connect(addr).is_err(),
            "the listener must be gone after drop"
        );
    }

    #[test]
    fn an_idle_connected_client_is_still_served() {
        let server = TestServer::start().expect("loopback is available");
        let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");
        thread::sleep(Duration::from_millis(500));

        assert_holding_float_is_fifty(&mut stream);
    }
}
