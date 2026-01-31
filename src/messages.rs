use serde::{Deserialize, Serialize};
use iroh::PublicKey;

use std::collections::HashMap;
use std::time::SystemTime;

#[derive(Serialize,Deserialize,Clone,Hash,Debug,PartialEq,Eq)]
pub struct VPFSNode {
    pub name: String,
    pub endpoint_id: PublicKey
}

#[derive(Debug,Clone,Eq,Hash,PartialEq,Serialize,Deserialize)]
pub struct Location {
    pub node_name: String,
    pub uri: String
}

#[derive(Serialize,Deserialize,Clone,Eq,Hash,PartialEq,Debug)]
pub struct DirectoryEntry {
    pub location: Location,
    pub name: String,
    pub is_dir: bool
}

#[derive(Serialize,Deserialize,Clone,Eq,Hash,PartialEq,Debug)]
pub struct CacheEntry {
    pub uri: String
}

/// Hello messages
#[derive(Serialize,Deserialize)]
pub enum Hello {
    ClientHello,
    DaemonHello,
    RootHello(VPFSNode),
}

/// Responses to Hello messages
#[derive(Serialize,Deserialize)]
pub enum HelloResponse {
    /// node_name
    ClientHello(String),
    DaemonHello,
    /// node, knownhosts
    RootHello(VPFSNode, HashMap<String, PublicKey>),
}

#[derive(Serialize,Deserialize,Debug,Eq,PartialEq)]
pub enum VPFSError {
    OnlyInCache(Location),
    CacheNeededForTraversal(DirectoryEntry),
    NotModified,
    DoesNotExist,  // We can verify that the file does not exist
    NotFound,      // We can not find the file. File may or may not exist
    NotAccessible, // We can not access the node need to complete request
    NotADirectory,
    AlreadyExists(DirectoryEntry),
    FileNotOpen,
    Other(String),
}

/// Requests to a daemon from a daemon
#[derive(Serialize,Deserialize)]
pub enum DaemonRequest {
    Place,
    Open(String),
    Read(String, Option<SystemTime>),
    ReadFd(i32, usize),
    ReadLineFd(i32),
    Close(i32),
    Write(String),
    Remove(String),
    AppendDirectoryEntry(String, DirectoryEntry),
    /// to request for endpoint_id of node given node_name
    AddressFor(String),
}

/// Responses to a daemon from a daemon for requests
#[derive(Serialize,Deserialize)]
pub enum DaemonResponse {
    Place(String),
    Open(Result<i32, VPFSError>),
    Read(Result<(), VPFSError>),
    ReadFd(Result<(), VPFSError>),
    ReadLineFd(Result<(), VPFSError>),
    Close(Result<(), VPFSError>),
    /// usize is number of bytes written
    Write(Result<usize, VPFSError>),
    Remove(Result<(), VPFSError>),
    AppendDirectoryEntry(Result<(), VPFSError>),
    /// `endpoint_id` for node given name
    AddressFor(Option<PublicKey>)
}

/// Requests from client to daemon
#[derive(Serialize,Deserialize)]
pub enum ClientRequest {
    Find(String),
    /// parent dir uri, name
    Place(String, String),
    /// parent dir uri, name
    Mkdir(String, String), 
    Open(Location),
    ReadFd(Location, i32, usize),
    ReadLineFd(Location, i32),
    Close(String, i32),
    Read(Location),
    /// `Location`, number of bytes to write
    Write(Location, usize),
}

/// Response to client requests
#[derive(Serialize,Deserialize)]
pub enum ClientResponse {
    Find(Result<DirectoryEntry, VPFSError>),
    Place(Result<Location, VPFSError>),
    Mkdir(Result<Location, VPFSError>),
    Open(Result<i32,VPFSError>),
    ReadFd(Result<usize, VPFSError>),
    ReadLineFd(Result<usize, VPFSError>),
    Close(Result<(), VPFSError>),
    /// usize is number of bytes read
    Read(Result<usize, VPFSError>),
    /// usize is number of bytes written
    Write(Result<usize, VPFSError>),
}