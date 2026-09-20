//! Consumer contract for the published read-only API.
//!
//! Expected numbers are literals. Register values are synthetic; they are
//! not readings from a real device.

use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use modbus_read::{
    read_registers, ExceptionCode, ExchangeError, FunctionCode, ModbusError, ReadRequest,
    RegisterAddress, RegisterCount, RegisterKind, Scale, TransactionId, UnitId, MAX_REGISTERS,
};

#[cfg(feature = "test-server")]
use modbus_read::TestServer;
#[cfg(feature = "test-server")]
use std::net::TcpStream;

/// Hex spellings of the write function codes this crate must not send.
const WRITE_CODES: [&str; 5] = ["0x05", "0x06", "0x0f", "0x0F", "0x10"];

/// Holding read, transaction 1, unit 1, address 0, count 2.
const HOLDING_REQUEST: [u8; 12] = [
    0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01, 0x03, 0x00, 0x00, 0x00, 0x02,
];

/// The same request for input registers: function code 4 at byte 7.
const INPUT_REQUEST: [u8; 12] = [
    0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x01, 0x04, 0x00, 0x00, 0x00, 0x02,
];

/// Holding reply: MBAP + unit 1 + function 3 + byte count 4 + 0x00 0x2a 0x01 0x90.
const HOLDING_REPLY: [u8; 13] = [
    0x00, 0x01, 0x00, 0x00, 0x00, 0x07, 0x01, 0x03, 0x04, 0x00, 0x2a, 0x01, 0x90,
];

/// Exception reply: function 0x83, code 2.
const EXCEPTION_REPLY: [u8; 9] = [0x00, 0x01, 0x00, 0x00, 0x00, 0x03, 0x01, 0x83, 0x02];

fn holding_read(transaction: u16) -> ReadRequest {
    ReadRequest::new(
        TransactionId::new(transaction),
        UnitId::new(1).expect("unit 1 is addressed"),
        FunctionCode::ReadHoldingRegisters,
        RegisterAddress::new(0),
        RegisterCount::new(2).expect("two registers"),
    )
    .expect("a read inside the address space")
}

fn input_read(transaction: u16) -> ReadRequest {
    ReadRequest::new(
        TransactionId::new(transaction),
        UnitId::new(1).expect("unit 1 is addressed"),
        FunctionCode::ReadInputRegisters,
        RegisterAddress::new(0),
        RegisterCount::new(2).expect("two registers"),
    )
    .expect("a read inside the address space")
}

/// Exhaustive: a third variant does not compile until this match names its byte.
fn wire_byte(code: FunctionCode) -> u8 {
    match code {
        FunctionCode::ReadHoldingRegisters => 3,
        FunctionCode::ReadInputRegisters => 4,
    }
}

/// A device made of bytes. What is written is kept; the answer is queued.
/// The bytes are synthetic.
struct MemoryDevice {
    written: Vec<u8>,
    incoming: Cursor<Vec<u8>>,
}

impl MemoryDevice {
    fn answering(answer: Vec<u8>) -> Self {
        Self {
            written: Vec::new(),
            incoming: Cursor::new(answer),
        }
    }
}

impl Read for MemoryDevice {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.incoming.read(buf)
    }
}

impl Write for MemoryDevice {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
    let mut files = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            files.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

fn count_of(line: &str, brace: char) -> i32 {
    i32::try_from(line.matches(brace).count()).unwrap_or(i32::MAX)
}

fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with("/*")
}

fn comment_names_a_write(line: &str) -> bool {
    is_comment(line) && line.to_ascii_lowercase().contains("write")
}

fn is_hand_built_write_refusal(line: &str) -> bool {
    contains_spelling(line, "0x06") && line.contains("write_by_hand")
}

fn contains_spelling(line: &str, spelling: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find(spelling) {
        let after = at + spelling.len();
        let continues = rest[after..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_hexdigit());
        if !continues {
            return true;
        }
        rest = &rest[after..];
    }
    false
}

fn public_function_name(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.split_whitespace().any(|word| word == "pub") {
        return None;
    }
    let mut words = trimmed.split_whitespace();
    while let Some(word) = words.next() {
        if word == "fn" {
            let raw = words.next()?;
            return raw.split('<').next()?.split('(').next();
        }
    }
    None
}

fn dependency_crates(toml: &str) -> Vec<String> {
    let mut in_deps = false;
    let mut crates = Vec::new();
    for line in toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = trimmed == "[dependencies]";
            continue;
        }
        if in_deps && !trimmed.is_empty() && !trimmed.starts_with('#') {
            let name = trimmed.split('=').next().unwrap_or(trimmed).trim();
            crates.push(name.to_owned());
        }
    }
    crates
}

/// Production lines plus the two allowed write-code exceptions: a comment that
/// names a write this crate cannot spell, and the test-server's refusal of a
/// hand-built 0x06 frame. Other `#[cfg(test)]` lines are skipped so an MBAP
/// length of 0x06 is not mistaken for a write.
fn scanned_lines(source: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut in_test = false;
    let mut depth = 0_i32;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if !in_test && trimmed.starts_with("#[cfg(test)]") {
            in_test = true;
            depth = 0;
            continue;
        }
        if in_test {
            depth += count_of(line, '{') - count_of(line, '}');
            if depth <= 0 {
                in_test = false;
            }
            if is_hand_built_write_refusal(line) {
                lines.push((index + 1, line));
            }
            continue;
        }
        lines.push((index + 1, line));
    }
    lines
}

#[test]
fn function_code_has_exactly_the_two_read_bytes() {
    assert_eq!(FunctionCode::ReadHoldingRegisters.code(), 3);
    assert_eq!(FunctionCode::ReadInputRegisters.code(), 4);
    assert_eq!(wire_byte(FunctionCode::ReadHoldingRegisters), 3);
    assert_eq!(wire_byte(FunctionCode::ReadInputRegisters), 4);
}

#[test]
fn holding_read_encodes_to_the_named_frame() {
    let frame = holding_read(1).encode();

    assert_eq!(frame, HOLDING_REQUEST);
}

#[test]
fn input_read_differs_from_holding_only_at_the_function_code() {
    let holding = holding_read(1).encode();
    let input = input_read(1).encode();

    assert_eq!(holding, HOLDING_REQUEST);
    assert_eq!(input, INPUT_REQUEST);
    let differing: Vec<usize> = holding
        .iter()
        .zip(&input)
        .enumerate()
        .filter(|(_, (left, right))| left != right)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(differing, vec![7]);
    assert_eq!((holding[7], input[7]), (3, 4));
}

#[test]
fn holding_reply_decodes_to_the_named_registers() {
    let registers = holding_read(1).decode(&HOLDING_REPLY);

    assert_eq!(registers, Ok(vec![42, 400]));
}

#[test]
fn exception_0x83_code_2_is_illegal_data_address() {
    let refused = holding_read(1).decode(&EXCEPTION_REPLY);

    assert_eq!(
        refused,
        Err(ModbusError::Exception {
            function: 3,
            code: ExceptionCode::IllegalDataAddress,
        })
    );
}

#[test]
fn a_reply_with_a_different_transaction_id_is_a_mismatch() {
    let refused = holding_read(2).decode(&HOLDING_REPLY);

    assert_eq!(
        refused,
        Err(ModbusError::TransactionMismatch {
            expected: 2,
            found: 1,
        })
    );
}

#[test]
fn f32be_decodes_the_named_float() {
    assert_eq!(RegisterKind::F32Be.decode(&[0x4248, 0x0000]), Ok(50.0));
}

#[test]
fn i16_decodes_the_named_signed_word() {
    assert_eq!(RegisterKind::I16.decode(&[0xFF38]), Ok(-200.0));
}

#[test]
fn u32be_decodes_the_named_wide_integer() {
    assert_eq!(RegisterKind::U32Be.decode(&[0x0001, 0x86A0]), Ok(100_000.0));
}

#[test]
fn the_same_word_is_unsigned_or_signed_by_kind() {
    assert_eq!(RegisterKind::U16.decode(&[0xFFFF]), Ok(65_535.0));
    assert_eq!(RegisterKind::I16.decode(&[0xFFFF]), Ok(-1.0));
}

#[test]
fn a_tenth_scale_applies_and_zero_and_nan_are_errors() {
    let scaled = Scale::new(0.1).unwrap().apply(2500.0);

    assert!((scaled - 250.0).abs() < 1e-9);
    assert_eq!(Scale::new(0.0), Err(ModbusError::ZeroScale));
    assert_eq!(Scale::new(f64::NAN), Err(ModbusError::NonFiniteScale));
}

#[test]
fn register_count_ceiling_is_125() {
    assert_eq!(MAX_REGISTERS, 125);
    assert_eq!(
        RegisterCount::new(126),
        Err(ModbusError::TooManyRegisters {
            count: 126,
            max: 125,
        })
    );
    assert_eq!(RegisterCount::new(0), Err(ModbusError::EmptyRead));
}

#[test]
fn unit_id_refuses_broadcast_and_reserved() {
    assert_eq!(UnitId::new(0), Err(ModbusError::BroadcastUnit));
    assert_eq!(
        UnitId::new(250),
        Err(ModbusError::ReservedUnit { unit: 250 })
    );
    assert_eq!(UnitId::new(1).map(UnitId::get), Ok(1));
    assert_eq!(UnitId::new(247).map(UnitId::get), Ok(247));
    assert_eq!(UnitId::new(255).map(UnitId::get), Ok(255));
}

#[test]
fn read_registers_writes_the_request_and_returns_the_reply() {
    let request = holding_read(1);
    let mut device = MemoryDevice::answering(HOLDING_REPLY.to_vec());

    let registers = match read_registers(&mut device, &request) {
        Ok(words) => words,
        Err(ExchangeError::Modbus(err)) => panic!("modbus: {err}"),
        Err(ExchangeError::Io(err)) => panic!("io: {err}"),
    };

    assert_eq!(registers, vec![42, 400]);
    assert_eq!(device.written, HOLDING_REQUEST);
}

#[cfg(feature = "test-server")]
#[test]
fn test_server_serves_reads_and_refuses_a_hand_built_write() {
    let server = TestServer::start().expect("loopback is available");
    let mut stream = TcpStream::connect(server.addr()).expect("the server accepts");

    let holding = holding_read(1);
    let words = read_registers(&mut stream, &holding).expect("holding registers");
    let value = RegisterKind::F32Be.decode(&words).expect("two words");
    assert!((Scale::new(1.0).unwrap().apply(value) - 50.0).abs() < 1e-9);

    let input = ReadRequest::new(
        TransactionId::new(1),
        UnitId::new(1).expect("unit 1 is addressed"),
        FunctionCode::ReadInputRegisters,
        RegisterAddress::new(10),
        RegisterCount::new(1).expect("one register"),
    )
    .expect("a read inside the address space");
    let words = read_registers(&mut stream, &input).expect("input register");
    let value = RegisterKind::I16.decode(&words).expect("one word");
    assert!((Scale::new(0.1).unwrap().apply(value) - -20.0).abs() < 1e-9);

    // Hand-built function code 0x06. Invented bytes; this crate cannot name it.
    let write_by_hand: [u8; 12] = [0, 1, 0, 0, 0, 6, 1, 0x06, 0, 0, 0, 1];
    stream
        .write_all(&write_by_hand)
        .expect("the request is written");
    let mut reply = [0_u8; 9];
    stream.read_exact(&mut reply).expect("the device answers");
    assert_eq!(reply, [0, 1, 0, 0, 0, 3, 1, 0x86, 1]);
}

#[test]
fn source_and_manifest_stay_read_only() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_sources(&src);
    let names: Vec<String> = files
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    for expected in ["lib.rs", "frame.rs", "decode.rs"] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} was not among the scanned files: {names:?}"
        );
    }

    for path in &files {
        let source =
            fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        for (number, line) in scanned_lines(&source) {
            for spelling in WRITE_CODES {
                if !contains_spelling(line, spelling) {
                    continue;
                }
                assert!(
                    comment_names_a_write(line) || is_hand_built_write_refusal(line),
                    "{}:{number} spells {spelling:?}\n{line}",
                    path.display()
                );
            }
            if let Some(name) = public_function_name(line) {
                assert!(
                    !(name.starts_with("set_")
                        || name.starts_with("cmd_")
                        || name.starts_with("write_")),
                    "{}:{number} is a public {name}\n{line}",
                    path.display()
                );
            }
        }
    }

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let toml = fs::read_to_string(&manifest)
        .unwrap_or_else(|err| panic!("read {}: {err}", manifest.display()));
    assert_eq!(dependency_crates(&toml), vec!["thiserror".to_owned()]);
}
