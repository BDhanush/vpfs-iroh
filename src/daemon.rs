use clap::Parser;
use iroh::{Endpoint, PublicKey, protocol::Router};
use serde::de::DeserializeOwned;
use serde::{Serialize};
use lru::LruCache;
use tokio::runtime::Handle;
use anyhow::Result;

use std::thread;
use std::net::{TcpListener, TcpStream};
use std::fs;
use std::io;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, RwLock};
use std::collections::HashMap;

mod protocol;
use crate::protocol::VPFSProtocol;

mod state;
use crate::state::DaemonState;

mod messages;
use messages::*;

mod remote_communication;
use remote_communication::*;

mod file_system;
use file_system::*;

#[derive(Parser, Debug)]
#[command(name = "vpfs", about = "Virtual private file system iroh prototype.")]
struct Opt {
    #[arg(short, long, default_value_t = 8080)]
    port: u16,

    #[arg(short, long)]
    listen_port: u16,

    #[arg(short, long)]
    root_id: Option<PublicKey>,

    //Maximum cache size in bytes
    #[arg(short, long, default_value_t = 1 << 16)]
    cache_size: usize,

    #[arg(short, long)]
    name: String
}

/// Send a message to a TcpStream
fn send_message_tcp <T: Serialize>(stream: &mut TcpStream, message: T) {
    serde_bare::to_writer(stream, &message).unwrap();
}

/// Receive a message from a TcpStream
fn receive_message_tcp <T: DeserializeOwned>(stream: &mut TcpStream) -> Result<T, serde_bare::error::Error> {
    serde_bare::from_reader(stream)
}

/// Handle client Find request
async fn handle_client_find(stream: &mut TcpStream, file: &str, state: &Arc<DaemonState>) {
    send_message_tcp(stream, ClientResponse::Find(recursive_find(file, state).await));
}

/// Handle client Place request
async fn handle_client_place(stream: &mut TcpStream, file: &str, node_name: String, state: &Arc<DaemonState>) {
    send_message_tcp(stream, ClientResponse::Place(place_file(file, &node_name, false, state).await));
}

/// Handle client Mkdir request
async fn handle_client_mkdir(stream: &mut TcpStream, directory: &str, node_name: String, state: &Arc<DaemonState>) {
    send_message_tcp(stream, ClientResponse::Mkdir(place_file(directory, &node_name, true, state).await));
}

/// Handle client Read request
/// <br>
async fn handle_client_read(stream: &mut TcpStream, location: Location, state: &Arc<DaemonState>) {
    // if file is local, read locally, else read remotely and send response back through stream
    if location.node_name == state.local.name {
        if let Ok(buf) = read_local(&location.uri, &state.file_access_lock) {
            send_message_tcp(stream, ClientResponse::Read(Ok(buf.len())));                    
            stream.write_all(&buf);
        } else {
            send_message_tcp(stream, ClientResponse::Read(Err(VPFSError::DoesNotExist)));
        }
    } else  {
        match read_remote(&location, state).await {
            Ok(buf) => {
                send_message_tcp(stream, ClientResponse::Read(Ok(buf.len())));                    
                stream.write_all(&buf);
            }
            Err(error) => {
                send_message_tcp(stream, ClientResponse::Read(Err(error)));
            }
        }
    }
}

/// Handle client Write request
async fn handle_client_write(stream: &mut TcpStream, location: Location, file_len: usize, state: &Arc<DaemonState>) {
    if location.node_name == state.local.name {
        let mut buf = vec![0u8;file_len];
        stream.read_exact(buf.as_mut()).unwrap();
        if write_local(&location.uri, &buf, &state.file_access_lock).is_ok() {
            send_message_tcp(stream, ClientResponse::Write(Ok(file_len)));
        } else {
            send_message_tcp(stream, ClientResponse::Write(Err(VPFSError::DoesNotExist)));
        }
    } else if let Some(file_owner_connection) = stream_for(&location.node_name, &state).await {
        let mut file_owner_connection = file_owner_connection.lock().unwrap();
        match file_owner_connection.open_bi().await {
            Ok((mut send, mut recv)) => {
                
                let mut buf = vec![0u8; file_len];
                stream.read_exact(&mut buf);
                send_message(&mut send, DaemonRequest::Write(location.uri)).await;
                send_message(&mut send, buf).await;
                if let Ok(DaemonResponse::Write(write_result)) = receive_message(&mut recv).await {
                    drop(file_owner_connection);
                    send_message_tcp(stream, ClientResponse::Write(write_result));
                }
                
            }
            Err(e) => eprintln!("✗ Error opening bi-directional stream: {}", e),
        }
    } else {
        send_message_tcp(stream, ClientResponse::Write(Err(VPFSError::NotAccessible)));
    }
}

/// Handle requests from connected client program
fn handle_client(mut stream: TcpStream, state: Arc<DaemonState>, rt_handle: &Handle) {
    rt_handle.block_on(async {
        loop {
            match receive_message_tcp(&mut stream) {
                Ok(ClientRequest::Find(file)) => {
                    handle_client_find(&mut stream, &file, &state).await;
                },
                Ok(ClientRequest::Place(file, node_name )) => {
                    handle_client_place(&mut stream, &file, node_name,  &state).await;
                }
                Ok(ClientRequest::Mkdir(directory, node_name )) => {
                    handle_client_mkdir(&mut stream, &directory, node_name, &state).await;
                }
                Ok(ClientRequest::Read(location)) => {
                    handle_client_read(&mut stream, location, &state).await;
                }
                Ok(ClientRequest::Write(location,len)) => {
                    handle_client_write(&mut stream, location, len, &state).await;
                }
                Err(_) => {
                    println!("Client diconnected");
                    break;
                }
            }
        }
    });
}

/// Handle incoming connection from client program
fn handle_connection(mut stream: TcpStream, state: Arc<DaemonState>, rt_handle: Handle) {
    match receive_message_tcp(&mut stream) {
        Ok(Hello::ClientHello) => {
            println!("User process connected");
            send_message_tcp(&mut stream, HelloResponse::ClientHello(state.local.name.clone()));
            handle_client(stream, state, &rt_handle);
        },
        Ok(_) => eprintln!("Unexpected hello message"),
        Err(_) => eprintln!("Did not receive proper hello message"),
    }
}

/// Start TCP server to accept connections from client programs
fn start_server(address: &str, state: Arc<DaemonState>, rt_handle: Handle) {
    let listener = TcpListener::bind(address).unwrap();
    println!("Listening for client connections");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                println!("Incoming connections");
                let state_clone = state.clone();
                let rt_handle_clone = rt_handle.clone();
                thread::spawn(move || {
                    handle_connection(stream, state_clone, rt_handle_clone); 
                });
            }
            Err(e) => {
                eprintln!("Connection failed: {}", e);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let opt = Opt::parse();
    
    // initialize iroh endpoint and wait for it to be online
    let address = format!("0.0.0.0:{}", opt.port);
    // let mut config = TransportConfig::default();
    // config.max_idle_timeout(None);
    let endpoint: Endpoint = Endpoint::builder()
        // .transport_config(config)
        .bind_addr_v4(address.parse().unwrap())
        .bind()
        .await?;
    
    endpoint.online().await;
    
    let endpoint_id = endpoint.id();
    println!("Endpoint Id: {endpoint_id}");

    // initialize daemon state
    let mut state = DaemonState {
        endpoint: endpoint.clone(),
        root: if let Some(root_id) = opt.root_id {
            RwLock::new(Some(VPFSNode{name: "root".to_string(), endpoint_id: root_id}))
        } else {
            RwLock::new(Some(VPFSNode{name: opt.name.clone(), endpoint_id: endpoint_id}))
        },
        local: VPFSNode{name: opt.name.clone(), endpoint_id},
        connections: Mutex::new(HashMap::new()),
        known_hosts: Mutex::new(None),
        cache: Mutex::new(LruCache::unbounded()),
        max_cache_size: opt.cache_size,
        used_cache_bytes: RwLock::new(0),
        file_access_lock: RwLock::new(())
    };
    
    setup_files_dir();
    
    restore_cache(&mut state);

    let state = Arc::new(state);

    // Initialize protocol router
    let router = Router::builder(endpoint)
        .accept(VPFSProtocol::ALPN, protocol::VPFSProtocol{ state:state.clone() })
        .spawn();

    if opt.root_id.is_some() {
        // root_id is provided, connect to root node, send hello and populate known hosts
        println!("Running as non root node");

        let remote_id = opt.root_id.unwrap();
        println!("Connecting to root node: {}", remote_id);
        let endpoint_addr = iroh::EndpointAddr::new(remote_id);

        match router.endpoint().connect(endpoint_addr, VPFSProtocol::ALPN).await {
            Ok(conn) => {
                println!("Connected to root node: {remote_id}");
                match conn.open_bi().await {
                    Ok((mut send, mut recv)) => {
                        println!("Opened bi-directional stream to root node: {}", remote_id);
                        
                        let msg = Hello::RootHello(state.local.clone());
                        send_message(&mut send, msg).await?;

                        println!("Sent hello to root node, waiting for response...");
                        
                        if let Ok(HelloResponse::RootHello(root_node, host_names)) = receive_message(&mut recv).await {
                            let mut known_hosts = state.known_hosts.lock().unwrap();
                            *known_hosts = Some(host_names);
                            known_hosts.as_mut().unwrap().insert(root_node.name.clone(), remote_id);
                            // println!("{}",root_node.name);
                            // println!("{:?}", known_hosts.as_ref().unwrap());
                            state.root.write().unwrap().replace(root_node);
                        } else {
                            eprintln!("✗ Failed to deserialize response from root node");
                        }
                        
                    }
                    Err(e) => eprintln!("✗ Error opening bi-directional stream: {}", e),
                }
            }
            Err(e) => {
                eprintln!("✗ Failed to connect to root node: {}", e);
                eprintln!("Error details: {:?}", e);
            }
        }
    } else {
        // current node is the root node
        // initialize known hosts map, create root directory if it does not exist, and add self links
        println!("Running as root node");

        state.known_hosts.lock().unwrap().replace(HashMap::new());
        if let Err(create_error) = fs::File::create_new("root") {
            if create_error.kind() != io::ErrorKind::AlreadyExists {
                panic!("Could not create root directory");
            }
        } else {
            let mut self_link = DirectoryEntry {
                location: Location { node_name: state.local.name.clone(), uri: "root".to_string() },
                name: ".".to_string(),
                is_dir: true
            };
            let _ = append_dir_entry("root", &self_link, &state);
            self_link.name = "..".to_string();
            let _ = append_dir_entry("root", &self_link, &state);
        }

    }

    let client_address = format!("0.0.0.0:{}",opt.listen_port);
    let rt_handle = Handle::current();
    start_server(&client_address, state.clone(), rt_handle);

    Ok(())

}