use anyhow::{Result};
use iroh::{
    endpoint::{Connection}, protocol::{ProtocolHandler}
};

use std::sync::Arc;
use std::fs;
use std::io::{Read, Seek, SeekFrom};

use crate::state::DaemonState;
use crate::messages::*;
use crate::file_system::*;
use crate::remote_communication::*;

#[derive(Debug, Clone)]
pub struct VPFSProtocol {
    pub state: Arc<DaemonState>
}

impl VPFSProtocol {
    pub const ALPN: &'static [u8] = b"uic/vpfs";

    /// Handle daemon requests
    async fn handle_daemon(&self, conn: Arc<Connection>) {
        let remote_id = conn.remote_id();

        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            match receive_message(&mut recv).await {
                Ok(DaemonRequest::Place)  => {
                    println!("Received Place request for node: {}", remote_id);
                    let response = DaemonResponse::Place(create_file_with_random_uri());
                    send_message(&mut send, response).await;
                }
                Ok(DaemonRequest::Open(uri)) => {
                    println!("Received Open request for node: {}", remote_id);

                    match open_file_local(&uri, &self.state.open_files) {
                        Ok(daemon_fd) => {
                            send_message(&mut send, DaemonResponse::Open(Ok(daemon_fd))).await;
                        }
                        Err(_) => {
                            send_message(&mut send, DaemonResponse::Open(Err(VPFSError::DoesNotExist))).await;
                        }
                    }
                }
                Ok(DaemonRequest::ReadFd(fd, len)) => {
                    match read_fd_local(fd, len, &self.state.open_files) {
                        Ok(buf) => {
                            send_message(&mut send, DaemonResponse::ReadFd(Ok(()))).await;
                            send_message(&mut send, buf).await;
                        }
                        Err(_) => {
                            send_message(&mut send, DaemonResponse::ReadFd(Err(VPFSError::FileNotOpen))).await;
                        }
                    }
                }
                Ok(DaemonRequest::ReadLineFd(fd)) => {
                    match read_line_fd_local(fd, &self.state.open_files) {
                        Ok(buf) => {
                            send_message(&mut send, DaemonResponse::ReadLineFd(Ok(()))).await;
                            send_message(&mut send, buf).await;
                        }
                        Err(_) => {
                            send_message(&mut send, DaemonResponse::ReadLineFd(Err(VPFSError::FileNotOpen))).await;
                        }
                    }
                }
                Ok(DaemonRequest::Close(fd)) => {
                    match close_file_local(fd, &self.state.open_files) {
                        Ok(()) => {
                            send_message(&mut send, DaemonResponse::Close(Ok(()))).await;
                        }
                        Err(_) => {
                            send_message(&mut send, DaemonResponse::Close(Err(VPFSError::FileNotOpen))).await;
                        }
                    }
                }
                Ok(DaemonRequest::Read( uri, last_modified )) => {
                    println!("Received Read request for node: {}", remote_id);

                    let should_send = {
                        if let Some(remote_last_modified) = last_modified {
                            let _fs_lock = self.state.file_system.read().unwrap();
                            if let Ok(file_data) = fs::metadata(&uri) {
                                if let Ok(local_last_modified) = file_data.modified() {
                                    local_last_modified >= remote_last_modified
                                } else { true }
                            } else { true }
                        } else {
                            true
                        }
                    };

                    if !should_send {
                        send_message(&mut send, DaemonResponse::Read(Err(VPFSError::NotModified))).await;
                        continue;
                    }

                    match read_local(&uri, &self.state.file_system) {
                        Ok(buf) => {
                            send_message(&mut send, DaemonResponse::Read(Ok(()))).await;
                            send_message(&mut send, buf).await;
                        }
                        Err(_) => {
                            send_message(&mut send, DaemonResponse::Read(Err(VPFSError::DoesNotExist))).await;
                        }
                    }
                }
                Ok(DaemonRequest::Write(uri)) => {
                    println!("Received Write request for node: {}", remote_id);

                    let buf=receive_message::<Vec<u8>>(&mut recv).await.unwrap();
                    if write_local(&uri, &buf, &self.state.file_system).is_ok() {
                        send_message(&mut send, DaemonResponse::Write(Ok(buf.len()))).await;
                    } else {
                        send_message(&mut send, DaemonResponse::Write(Err(VPFSError::DoesNotExist))).await;
                    }
                }
                Ok(DaemonRequest::Remove(uri)) => {
                    let result = {
                        let _fs_lock = self.state.file_system.write().unwrap();
                        fs::remove_file(uri).is_ok()
                    };

                    if result {
                        send_message(&mut send, DaemonResponse::Remove(Ok(()))).await;
                    } else {
                        send_message(&mut send, DaemonResponse::Remove(Err(VPFSError::DoesNotExist))).await;
                    }
                }
                Ok(DaemonRequest::AddEntry(path, file_entry)) => {
                    println!("Received AddEntry request for node: {}", remote_id);

                    place_file_in_memory(&self.state.file_system, &path, file_entry);
                    send_message(&mut send, DaemonResponse::AddEntry(Ok(()))).await;
                }
                Ok(DaemonRequest::AddressFor(node_name)) => {
                    let addr = {
                        let known_nodes = self.state.known_nodes.lock().unwrap();
                        known_nodes.get(&node_name).cloned()
                    };

                    send_message(&mut send, DaemonResponse::AddressFor(addr)).await;
                }
                Ok(DaemonRequest::FileSystem) => {
                    println!("Received FileSystem request for node: {}", remote_id);

                    let data = {
                        let file_system = self.state.file_system.read().unwrap();
                        file_system.clone()
                    };
                    send_message(&mut send, DaemonResponse::FileSystem(data)).await;
                }
                Ok(_) => eprintln!("Unexpected message from {remote_id}"),
                Err(e) => eprintln!("Error receiving message from {remote_id}: {:?}", e),
            }
                
        }
    }

    /// Handle an incoming iroh connection
    pub async fn handle_connection(&self, conn: Connection) {
        let conn = Arc::new(conn);
        let remote_id = conn.remote_id();
        println!("Accepted connection from {remote_id}");

        if let Ok((mut send, mut recv)) = conn.accept_bi().await {
            println!("Opened bi-directional stream, endpoint id: {}", remote_id);

            match receive_message(&mut recv).await {
                Ok(Hello::DaemonHello(node)) => {
                    println!("Received DaemonHello from node: {}, endpoint_id: {}", node.name, node.endpoint_id);
                    {    
                        let mut known_nodes = self.state.known_nodes.lock().unwrap();
                        known_nodes.insert(node.name.clone(), node.endpoint_id.clone());
                        let mut connections = self.state.connections.lock().unwrap();
                        connections.insert(node.name.clone(), conn.clone());
                    }
                    for (name, connection) in self.state.connections.lock().unwrap().iter() {
                        println!("connection: {}, {:?}", name, connection.close_reason());
                    }
                    send_message(&mut send, HelloResponse::DaemonHello).await;
                    self.handle_daemon(conn).await;
                }
                Ok(Hello::InitHello(new_nodes)) => {
                    println!("Received InitHello from node: {}, new nodes: {:?}", remote_id, new_nodes);

                    let known_nodes_snapshot = {
                        let mut known_nodes = self.state.known_nodes.lock().unwrap();
                        let mut known_nodes_snapshot = known_nodes.clone();
                        known_nodes.extend(new_nodes);
                        known_nodes_snapshot.insert(self.state.local.name.clone(), self.state.local.endpoint_id.clone());
                        known_nodes_snapshot
                    };

                    send_message(&mut send, HelloResponse::InitHello(known_nodes_snapshot)).await;
                    self.handle_daemon(conn).await;
                }
                Ok(_) => eprintln!("Unexpected message from {remote_id}"),
                Err(e) => eprintln!("Error receiving message from {remote_id}: {:?}", e),
            }
                
        }
    }
}

impl ProtocolHandler for VPFSProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), iroh::protocol::AcceptError> {
        self.handle_connection(conn).await;
        Ok(())
    }
}

// impl ProtocolHandler for VPFSProtocol {
//     fn accept(&self, conn: Connection) -> impl Future<Output = Result<(), AcceptError>> + Send {
//         Box::pin(async move {
//             self.handle_connection(conn).await;
//             Ok(())
//         })
//     }
// }