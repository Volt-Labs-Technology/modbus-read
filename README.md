# modbus-read

## What this is

A read-only Modbus TCP client. Modbus TCP is a request/response protocol used by electricity meters and power-distribution units: a header (transaction id, protocol id, length, unit id) sits in front of a short body. This crate encodes a read, decodes the reply, and turns the 16-bit register words into numbers. Only function codes 3 (read holding registers) and 4 (read input registers) exist in the types, so a write cannot be expressed.

## Who it is for

An engineer with a meter or similar device on a TCP network who needs to read registers from Rust without taking a dependency that can write.

## How to use it

Add the crate to a Rust project:

```sh
cargo add modbus-read
```

That writes this line into `Cargo.toml`:

```toml
modbus-read = "0.1.0"
```

Minimal example. The register values below are synthetic; they are not readings from a real device.

```rust,no_run
use std::net::TcpStream;

use modbus_read::{
    read_registers, FunctionCode, ReadRequest, RegisterAddress, RegisterCount,
    TransactionId, UnitId,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect("127.0.0.1:502")?;
    let request = ReadRequest::new(
        TransactionId::new(1),
        UnitId::new(1)?,
        FunctionCode::ReadHoldingRegisters,
        RegisterAddress::new(0),
        RegisterCount::new(2)?,
    )?;
    let words = read_registers(&mut stream, &request)?;
    println!("{words:?}");
    Ok(())
}
```

A loopback device that serves fixed synthetic registers is available behind the `test-server` feature. See `examples/read_two_registers.rs`.

## What it deliberately does not do

It does not write to a device. It does not speak Modbus RTU over a serial line. It does not retry or poll; the caller owns the connection, the schedule, and the next transaction id.

Version 0.x: the API may change before 1.0.
