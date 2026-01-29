use anyhow::{Result};
use iroh::{
    endpoint::{Connection}, protocol::{ProtocolHandler}
};

use std::sync::Arc;
use std::fs;

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
    async fn handle_daemon(&self, mut conn:Connection) {
        let remote_id = conn.remote_id();

        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            match receive_message(&mut recv).await {
                Ok(DaemonRequest::Place)  => {
                    let response = DaemonResponse::Place(create_file_with_random_uri());
                    send_message(&mut send, response).await;
                }
                Ok(DaemonRequest::Open(uri)) => {
                    match open_file_local(&uri, &self.state.open_files) {
                        Ok(daemon_fd) => {
                            send_message(&mut send, DaemonResponse::Open(Ok(daemon_fd))).await;
                        }
                        Err(_) => {
                            send_message(&mut send, DaemonResponse::Open(Err(VPFSError::DoesNotExist))).await;
                        }
                    }
                }
                Ok(DaemonRequest::Read( uri, last_modified )) => {
                    let should_send = {
                        if let Some(remote_last_modified) = last_modified {
                            let fs_lock = self.state.file_access_lock.read().unwrap();
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

                    match read_local(&uri, &self.state.file_access_lock) {
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
                    let buf=receive_message::<Vec<u8>>(&mut recv).await.unwrap();
                    if write_local(&uri, &buf, &self.state.file_access_lock).is_ok() {
                        send_message(&mut send, DaemonResponse::Write(Ok(buf.len()))).await;
                    } else {
                        send_message(&mut send, DaemonResponse::Write(Err(VPFSError::DoesNotExist))).await;
                    }
                }
                Ok(DaemonRequest::AppendDirectoryEntry(directory,new_entry )) => {
                    send_message(&mut send, DaemonResponse::AppendDirectoryEntry(append_dir_entry(&directory, &new_entry, &self.state))).await;
                }
                Ok(DaemonRequest::Remove(uri)) => {
                    let result = {
                        let _fs_lock = self.state.file_access_lock.write().unwrap();
                        fs::remove_file(uri).is_ok()
                    };

                    if result {
                        send_message(&mut send, DaemonResponse::Remove(Ok(()))).await;
                    } else {
                        send_message(&mut send, DaemonResponse::Remove(Err(VPFSError::DoesNotExist))).await;
                    }
                }
                Ok(DaemonRequest::AddressFor(node_name)) => {
                    let addr = {
                        let known_hosts_lock = self.state.known_hosts.lock().unwrap();
                        known_hosts_lock
                            .as_ref()
                            .and_then(|kh| kh.get(&node_name).cloned())
                    };

                    send_message(&mut send, DaemonResponse::AddressFor(addr)).await;
                }
                Ok(_) => eprintln!("Unexpected message from {remote_id}"),
                Err(e) => eprintln!("Error receiving message from {remote_id}: {:?}", e),
            }
                
        }
    }

    /// Handle an incoming iroh connection
    pub async fn handle_connection(&self, mut conn: Connection) {
        let remote_id = conn.remote_id();
        println!("Accepted connection from {remote_id}");

        if let Ok((mut send, mut recv)) = conn.accept_bi().await {
            println!("Opened bi-directional stream, endpoint id: {}", remote_id);

            match receive_message(&mut recv).await {
                Ok(Hello::DaemonHello) => {
                    send_message(&mut send, HelloResponse::DaemonHello).await;
                    self.handle_daemon(conn).await;
                }
                Ok(Hello::RootHello(connecting_node)) => {
                    let (root_node, known_hosts_snapshot) = {
                        let mut known_hosts = self.state.known_hosts.lock().unwrap();
                        known_hosts.as_mut().unwrap().insert(connecting_node.name.clone(), remote_id);

                        let root_guard = self.state.root.read().unwrap();

                        (root_guard.clone().unwrap(), known_hosts.clone().unwrap())
                        // all locks dropped here else we'll have locks set in await fn
                    };

                    send_message(&mut send, HelloResponse::RootHello(root_node, known_hosts_snapshot)).await;
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