use crate::command::Command;
use std::io;
use std::io::prelude::*;
use std::net::TcpListener;

pub struct SimpleTcpListener {
    listener: TcpListener,
}

impl SimpleTcpListener {
    pub fn new(ip: &'static str) -> SimpleTcpListener {
        SimpleTcpListener {
            listener: TcpListener::bind(ip).unwrap(),
        }
    }

    pub fn echo(&self) -> io::Result<()> {
        let (mut socket, _addr) = self.listener.accept().unwrap();
        let mut head = [0; 8];
        socket.read_exact(&mut head)?;

        let bl = Command::buffer_length(head[2], head[3]);
        let mut buf = vec![0; bl];
        socket.read_exact(&mut buf)?;
        socket.write_all(&[&head, &buf[..]].concat())?;
        Ok(())
    }

    pub fn mock_server(&self, responses: Vec<Vec<u8>>) -> io::Result<()> {
        let (mut stream, _addr) = self.listener.accept().unwrap();
        let mut count = 0;

        while count < responses.len() {
            let mut head = [0; 8];
            stream.read_exact(&mut head)?;
            let bl = Command::buffer_length(head[2], head[3]);
            let mut buf = vec![0; bl];
            stream.read_exact(&mut buf)?;
            stream.write_all(&responses[count])?;
            count += 1;
        }

        Ok(())
    }
}
