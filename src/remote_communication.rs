use iroh::Endpoint;
use iroh::endpoint::Connection;
use iroh::endpoint::RecvStream;
use iroh::endpoint::SendStream;
use serde::de::DeserializeOwned;
use serde::Serialize;
use anyhow::Result;

use std::sync::{Arc, Mutex};

use crate::protocol::VPFSProtocol;
use crate::messages::{Hello, HelloResponse};

use crate::state::DaemonState;
use crate::messages::{DaemonRequest, DaemonResponse, VPFSNode};

pub async fn send_message<T: serde::Serialize>(send: &mut SendStream, msg: T) -> Result<()> {
    // Serialize message
    let buf = serde_bare::to_vec(&msg)?;

    // Write length
    send.write_all(&(buf.len() as u64).to_be_bytes()).await?;
    // Write payload
    send.write_all(&buf).await?;
    // send.finish()?;

    Ok(())
}

pub async fn receive_message<T: DeserializeOwned>(recv: &mut RecvStream) ->  Result<T> {
    // Read length
    let mut len_buf = [0u8; 8];
    recv.read_exact(&mut len_buf).await?;
    let len = u64::from_be_bytes(len_buf) as usize;

    // Read payload
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;

    // Deserialize message
    let msg = serde_bare::from_slice(&buf)?;
    Ok(msg)
}

pub async fn send_and_receive <T: Serialize, U: DeserializeOwned> (node_name: &String, message: T, state: &Arc<DaemonState>) -> Result<U, anyhow::Error> {
    if let Some(node_connection_lock) = stream_for(node_name, state).await {
        let mut node_connection = node_connection_lock.lock().unwrap();
        if let Ok((mut send, mut recv)) = node_connection.open_bi().await {
            send_message(&mut send, message).await;
            return receive_message(&mut recv).await;
        }
        
    }
    Err(anyhow::Error::msg("Could not connect"))
    
}

async fn establish_connection(endpoint: &Endpoint, node: &VPFSNode) -> Option<Connection> {
    let remote_id = node.endpoint_id;
    println!("Connecting to root node: {}", remote_id);
    // connect to the other endpoint
    let endpoint_addr = iroh::EndpointAddr::new(remote_id);
    match endpoint.connect(endpoint_addr, VPFSProtocol::ALPN).await {
        Ok(conn) => {
            println!("Connected to root node: {remote_id}");
            match conn.open_bi().await {
                Ok((mut send, mut recv)) => {
                    println!("Opened bi-directional stream to root node: {}", remote_id);

                    send_message(&mut send, Hello::DaemonHello).await;
                    receive_message::<HelloResponse>(&mut recv).await.expect("Got bad hello response");

                    println!("Sent hello to root node, waiting for response...");
                    
                    return Some(conn);
                }
                Err(e) => {
                    eprintln!("Error opening bi-directional stream: {}", e);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to connect to node: {}", e);
        }
    }

    None
}

pub async fn stream_for(node_name: &String, state: &Arc<DaemonState>) -> Option<Arc<Mutex<Connection>>> {
    let mut connections = state.connections.lock().unwrap();
    if let Some(connection) = connections.get(node_name) {
        return Some(connection.clone());
    }
    let known_hosts = state.known_hosts.lock().unwrap();
    if let Some(remote_id) = known_hosts.as_ref().unwrap().get(node_name) {
        if let Some(conn) = establish_connection(&state.endpoint, &VPFSNode{name: node_name.clone(), endpoint_id:remote_id.clone()}).await {
            let conn = Arc::new(Mutex::new(conn));
            connections.insert(node_name.clone(), conn.clone());
            return Some(conn);
        }
    }
    if let Some(root_node) = state.root.read().unwrap().as_ref() {
        if state.local == *root_node {
            return None;
        }
        if let Some(root_connection) = connections.get(&root_node.name) {
            let mut root_connection = root_connection.lock().unwrap();
            match root_connection.open_bi().await {
                Ok((mut send, mut recv)) => {
                    println!("Opened bi-directional stream to root node: {}", root_node.endpoint_id);
                    
                    send_message(&mut send, DaemonRequest::AddressFor(node_name.clone())).await;
                    match receive_message(&mut recv).await {
                        Ok(DaemonResponse::AddressFor(Some(remote_id))) => {
                            drop(root_connection);
                            if let Some(conn) = establish_connection(&state.endpoint, &VPFSNode{name: node_name.clone(), endpoint_id:remote_id}).await {
                                let conn = Arc::new(Mutex::new(conn));
                                connections.insert(node_name.clone(), conn.clone());
                                return Some(conn);
                            }
                        },
                        _ => return None
                    }
                    
                }
                Err(e) => eprintln!("Error opening bi-directional stream: {}", e),
            }
            
        }
    }
    None
}