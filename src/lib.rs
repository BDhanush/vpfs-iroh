use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream};
use std::sync::{Arc, Mutex};

pub mod messages;
use messages::*;

pub struct VPFS {
    pub local: String, // name
    connection: Mutex<TcpStream>
}

impl VPFS {
    pub fn connect(listen_port: u16) -> Result<VPFS, std::io::Error> {
        let stream = TcpStream::connect(format!("localhost:{}", listen_port))?;

        serde_bare::to_writer(&stream, &Hello::ClientHello)?;
        let hello_response = serde_bare::from_reader::<_, HelloResponse>(&stream);
        if let Ok(HelloResponse::ClientHello(local_String)) = hello_response{
            let vpfs = VPFS { 
            local: local_String,
            connection: Mutex::new(stream),
            };
            Ok(vpfs)
        }
        else {
            panic!("Got wrong hello response");
        }
        
    }

    fn send_request_async(&self, stream: &TcpStream, req: ClientRequest) {
        serde_bare::to_writer(stream, &req).unwrap();
    }

    fn receive_response_async(&self, stream: &TcpStream) -> ClientResponse {
        let resp = serde_bare::from_reader(stream).unwrap();
        resp
    }

    fn send_request(&self, req: ClientRequest) -> ClientResponse {
        let stream = self.connection.lock().unwrap();
        serde_bare::to_writer(&mut &*stream, &req).unwrap();
        let resp = serde_bare::from_reader(&*stream).unwrap();
        resp
    }

    pub fn find(&self, path: &str) -> Result<DirectoryEntry, VPFSError> {
        if let ClientResponse::Find(find_result) = self.send_request(ClientRequest::Find(path.to_string())) {
            find_result
        }
        else {
            panic!("Bad response to find")
        }
    }

    pub fn place(&self, path: &str, at: String) -> Result<Location, VPFSError>{
        if let ClientResponse::Place(place_result) = self.send_request(ClientRequest::Place(path.to_string(), at)) {
            place_result
        }
        else {
            panic!("Bad response to place")
        }
    }

    pub fn mkdir(&self, path: &str, at: String) -> Result<Location, VPFSError>{
        if let ClientResponse::Mkdir(mkdir_result) = self.send_request(ClientRequest::Mkdir(path.to_string(), at)) {
            mkdir_result
        }
        else {
            panic!("Bad response to mkdir")
        }
    }

    pub fn read(&self, what: Location) -> Result<Vec<u8>, VPFSError> {
        let mut stream = self.connection.lock().unwrap();
        self.send_request_async(&stream, ClientRequest::Read(what));
        match self.receive_response_async(&stream) {
            ClientResponse::Read(Ok(len)) => {
                let mut buf=vec![0u8;len];
                stream.read_exact(&mut buf);
                Ok(buf)
            },
            ClientResponse::Read(Err(error)) => {
                Err(error)
            },
            _ => panic!("Bad response to read!"),
        }
    } 
    pub fn write(&self, what: Location, buf: &[u8]) -> Result<(), VPFSError> {
        let mut stream = self.connection.lock().unwrap();
        self.send_request_async(&stream, ClientRequest::Write(what, buf.len()));
        stream.write_all(buf);

        match self.receive_response_async(&stream) {
            ClientResponse::Write(Ok(len)) => {
                assert!(len == buf.len());
                Ok(())
            },
            ClientResponse::Write(Err(error)) => {
                Err(error)
            },
            _ => panic!("Bad response to write!"),
        }
    }

    pub fn fetch(&self, name: &str) -> Result<Vec<u8>, VPFSError> {
        let dir_entry = self.find(name)?;
        self.read(dir_entry.location)
    }

    pub fn store(&self, name: &str, buf: &[u8]) -> Result<(), VPFSError> {
        let location = match self.place(name, self.local.clone()) {
            Ok(location) => location,
            Err(VPFSError::AlreadyExists(dir_entry)) => dir_entry.location,
            Err(error) => return Err(error),
        };
        self.write(location.clone(), buf)
    }
}

