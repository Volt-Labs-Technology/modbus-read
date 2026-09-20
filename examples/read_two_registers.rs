//! Read two synthetic registers from the crate's test server on localhost.
//!
//! The register values are invented for this example. They are not readings
//! from a real device.

use std::net::TcpStream;

use modbus_read::{
    read_registers, FunctionCode, ReadRequest, RegisterAddress, RegisterCount, RegisterKind,
    TestServer, TransactionId, UnitId,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = TestServer::start()?;
    let mut stream = TcpStream::connect(server.addr())?;

    let request = ReadRequest::new(
        TransactionId::new(1),
        UnitId::new(1)?,
        FunctionCode::ReadHoldingRegisters,
        RegisterAddress::new(0),
        RegisterCount::new(2)?,
    )?;
    let words = read_registers(&mut stream, &request)?;
    let value = RegisterKind::F32Be.decode(&words)?;

    println!("synthetic holding registers 0 and 1: {words:?}");
    println!("decoded as f32be: {value}");
    Ok(())
}
