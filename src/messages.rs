use serde::{Deserialize, Serialize};
use iroh::PublicKey;

use std::collections::{HashMap, HashSet};
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

#[derive(Serialize,Deserialize)]
pub enum Hello {
    ClientHello,
    DaemonHello,
    RootHello(VPFSNode),
}

#[derive(Serialize,Deserialize)]
pub enum HelloResponse {
    ClientHello(String), // node name 
    DaemonHello,
    RootHello(VPFSNode, HashMap<String, PublicKey>), // node, knownhosts
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
    Other(String),
}

#[derive(Serialize,Deserialize)]
pub enum DaemonRequest {
    Place,
    Read(String, Option<SystemTime>),
    Write(String),
    Remove(String),
    AppendDirectoryEntry(String, DirectoryEntry),
    AddressFor(String) // node name
}

#[derive(Serialize,Deserialize)]
pub enum DaemonResponse {
    Place(String),
    Read(Result<(), VPFSError>),
    Write(Result<usize, VPFSError>),
    Remove(Result<(), VPFSError>),
    AppendDirectoryEntry(Result<(), VPFSError>),
    AddressFor(Option<PublicKey>)
}


#[derive(Serialize,Deserialize)]
pub enum ClientRequest {
    Find(String),
    Place(String, String), // parent dir uri, name
    Mkdir(String, String), // parent dir uri, name
    Read(Location),
    Write(Location, usize),
}

#[derive(Serialize,Deserialize)]
pub enum ClientResponse {
    Find(Result<DirectoryEntry, VPFSError>),
    Place(Result<Location, VPFSError>),
    Mkdir(Result<Location, VPFSError>),
    Read(Result<usize, VPFSError>),
    Write(Result<usize, VPFSError>),
}