use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::os::fd::{AsRawFd, IntoRawFd};
use std::result;
use std::{fs, io::{Read, Write}};
use std::sync::{Mutex, RwLock};
use std::io::{self, BufRead, BufReader};
use std::sync::Arc;
use iroh::endpoint::Connection;
use rand::Rng;
use lru::LruCache;
use rand::rand_core::le;

use std::sync::MutexGuard;

use crate::{file_system, messages::*};

use crate::state::DaemonState;

use crate::remote_communication::*;

/// Create ./files and go to it. Panic if it cannot be created or cd'ed into.
pub fn setup_files_dir() -> bool {
    if let Err(err) = fs::create_dir("./files") {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            std::env::set_current_dir("./files").expect("Could not cd into ./files directory");
            return false;
        }
        panic!("Could not create directory for storing files");
    }
    std::env::set_current_dir("./files").expect("Could not cd into ./files directory");
    true
}

pub fn add_cache_entry(file: &FileEntry, data: &[u8], cache: &mut MutexGuard<LruCache<FileEntry, CacheEntry>>, state: &Arc<DaemonState>) {
    if let Some(cache_entry) = cache.get(&file) {
        fs::write(&cache_entry.uri, &data);
    }
    else {
        let new_cache_entry = CacheEntry {
            uri: create_file_with_random_uri(),
        };
        fs::write(&new_cache_entry.uri, &data);
        cache.put(file.clone(), new_cache_entry);
    };
    let mut used_cache = state.used_cache_bytes.write().unwrap();
    *used_cache += data.len();
    // Evict elements to make room in cache
    while *used_cache > state.max_cache_size {
        if let Some((_, lru_entry)) = cache.pop_lru() {
            let file_size = fs::metadata(&lru_entry.uri).expect("Cache entry missing backing file").len();
            fs::remove_file(&lru_entry.uri).unwrap();
            *used_cache -= file_size as usize;
        }
        else {
            break;
        }
    }
    let cache_file = fs::File::create("cache").expect("Failed to create cache file");
    serde_bare::to_writer(&cache_file, &*used_cache).expect("Failed to save cahce size to file");
    for (key, value) in cache.iter() {
        serde_bare::to_writer(&cache_file, key).expect("Could not write cache entry to file");
        serde_bare::to_writer(&cache_file, value).expect("Could not write cache entry to file");
    }
}


/// Restore cache from ./cache file if it exists
pub fn restore_cache(state: &Arc<DaemonState>) {
    if let Ok(cache_file) = fs::File::open("cache") {
        let mut cache = state.cache.lock().unwrap();
        *state.used_cache_bytes.write().unwrap() = serde_bare::from_reader(&cache_file).expect("Failed to readed from cache file");
        while let Ok(key) = serde_bare::from_reader::<_, FileEntry>(&cache_file) {
            let value = serde_bare::from_reader(&cache_file).unwrap();
            cache.put(key.clone(), value);
            cache.demote(&key);
        }
    }
}

/// Restore file system from ./file_system file if it exists
pub fn restore_file_system(state: &Arc<DaemonState>) {
    if let Ok(fs_file) = fs::File::open("file_system") {
        let mut file_system = state.file_system.write().unwrap();
        while let Ok(path) = serde_bare::from_reader::<_, String>(&fs_file) {
            let entry: FileEntry = serde_bare::from_reader(&fs_file).unwrap();
            file_system.insert(path, entry);
        }
    }
}

pub fn read_local(uri: &str, file_system: &RwLock<HashMap<String, FileEntry>>) -> io::Result<Vec<u8>> {
    fs::read(uri).map_err(|_| io::Error::from(io::ErrorKind::NotFound))
}

pub fn write_local(uri: &str, data: &Vec<u8>, file_system: &RwLock<HashMap<String, FileEntry>>) -> io::Result<()> {
    let outer = file_system.read().unwrap();
    if outer.values().any(|e| e.uri == uri) {
        if fs::exists(uri)? {
            fs::write(uri, data)
        } else {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    } else {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }
}

pub fn create_file_with_random_uri() -> String {
    let mut rng = rand::rng();
    let mut uri = format!("{:x}", rng.random::<u64>());
    loop {
        if let Err(error) = fs::File::create_new(&uri) {
            if error.kind() != io::ErrorKind::AlreadyExists {
                panic!("Could not create file"); // TODO better error handleing
            }
            uri = format!("{:x}", rng.random::<u64>());
        }
        else {
            break;
        }
    }
    uri
}

pub async fn build_file_system(connection: Connection, state: &Arc<DaemonState>) {
    println!("Building file system from connection: {}", connection.remote_id());
    match connection.open_bi().await {
        Ok((mut send, mut recv)) => {
            let msg = DaemonRequest::FileSystem;
            send_message(&mut send, msg).await;

            match receive_message::<DaemonResponse>(&mut recv).await {
                Ok(DaemonResponse::FileSystem(data)) => {
                    let mut file_system = state.file_system.write().unwrap();
                    for (path, entry) in data {
                        file_system.entry(path).or_insert(entry);
                    }
                },
                Ok(_) => {
                    eprintln!("Unexpected response");
                }
                Err(e) => { eprintln!("Error: {}", e); }
            }

        }
        Err(e) => eprintln!("Error opening bi-directional stream: {}", e),
    }

}

pub async fn read_remote(file: &FileEntry, state: &Arc<DaemonState>) -> Result<Vec<u8>, VPFSError> {
    let mut cache = state.cache.lock().unwrap();
    let cache_entry = cache.get(&file);
    let _fs_lock = state.file_system.read().unwrap();
    let cache_last_update_time = if let Some(cache_entry) = cache_entry {
        if let Ok(file_data) = fs::metadata(&cache_entry.uri) {
            file_data.modified().ok()
        }
        else {
            None
        }
    }
    else {
        None
    };
    if let Some(file_owner_connection) = get_connection(&file.owner, state).await {
        match file_owner_connection.open_bi().await {
            Ok((mut send, mut recv)) => {
                send_message(&mut send, DaemonRequest::Read(file.uri.clone(), cache_last_update_time)).await;
                
                match receive_message(&mut recv).await {
                    Ok(DaemonResponse::Read(Ok(()))) => {

                        let buf = receive_message::<Vec<u8>>(&mut recv).await.unwrap();

                        add_cache_entry(&file, &buf, &mut cache, state);

                        return Ok(buf)
                    },
                    Ok(DaemonResponse::Read(Err(VPFSError::NotModified))) => {
                        return Ok(fs::read(&cache_entry.unwrap().uri).expect("Missing file for cache entry"))
                    }
                    Ok(DaemonResponse::Read(Err(error))) => {
                        return Err(error)
                    },
                    Ok(_) => panic!("Bad response"),
                    Err(_) => {
                        todo!("Check if error came from bad response, or from connection closing")
                    }
                }                
            }
            Err(e) => {
                eprintln!("Error opening bi-directional stream: {}", e);
                return Err(VPFSError::NotAccessible);
            }
        }
    }
    else {
        if let Some(cache_entry) =  cache_entry{
            let cache_entry_file = FileEntry {
                owner: state.local.name.clone(),
                uri: cache_entry.uri.clone(),
                name: file.name.clone()
            };
            Err(VPFSError::OnlyInCache(cache_entry_file))
        }
        else {
            Err(VPFSError::NotAccessible)
        }
    }
}

pub fn place_file_in_memory(file_system: &RwLock<HashMap<String, FileEntry>>, path: &str, new_file: FileEntry) {
    println!("Placing file in memory at path: {}, with uri: {}, owner: {}", path, new_file.uri, new_file.owner);
    let mut fs = file_system.write().unwrap();
    fs.insert(path.to_string(), new_file);
    let fs_file = fs::File::create("file_system").expect("Failed to create file_system file");
    for (path, entry) in fs.iter() {
        serde_bare::to_writer(&fs_file, path).expect("Failed to write path to file_system file");
        serde_bare::to_writer(&fs_file, entry).expect("Failed to write entry to file_system file");
    }
}

pub async fn place_file(path: &str, at: &String, state: &Arc<DaemonState>) -> Result<FileEntry, VPFSError>{
    let find_result = find(path, state);
    if find_result.is_ok() {
        return Err(VPFSError::AlreadyExists(find_result.unwrap()));
    }
    let uri = if *at == state.local.name {
        create_file_with_random_uri()
    }
    else if let Ok(DaemonResponse::Place(uri)) = send_and_receive(at, DaemonRequest::Place, state).await {
        uri
    }
    else {
        return Err(VPFSError::NotAccessible);
    };
    let new_file = FileEntry {
        owner: at.clone(),
        uri: uri,
        name: path.to_string(),
    };
    place_file_in_memory(&state.file_system, path, new_file.clone());

    let connections: Vec<Arc<Connection>> = {
        let conns = state.connections.lock().unwrap();
        conns.values()
            .filter(|c| c.close_reason().is_none())
            .cloned()
            .collect()
    };
    println!("Notifying {} other nodes of new file", connections.len());
    for conn in connections {
        if let Ok((mut send, mut recv)) = conn.open_bi().await {
            let _ = send_message(&mut send, DaemonRequest::AddEntry(path.to_string(), new_file.clone())).await;
            let _ = receive_message::<DaemonResponse>(&mut recv).await;
        }
    }

    Ok(new_file)
}


pub fn list_files(dir: &str, state: &Arc<DaemonState>) -> Result<Vec<FileEntry>, VPFSError> {
    // TODO
    // for now lists all files (as directories are not supported)
    let outer = state.file_system.read().unwrap();
    let entries = outer.iter()
        .map(|(_, entry)| entry.clone())
        .collect();
    Ok(entries)
}

pub fn find(file: &str, state: &Arc<DaemonState>) -> Result<FileEntry, VPFSError> {
    let outer = state.file_system.read().unwrap();
    println!("Finding file: {}, file system: {:?}", file, *outer);
    outer.get(file)
        .map(|e| e.clone())
        .ok_or(VPFSError::DoesNotExist)
}

pub fn open_file_local(uri: &str, open_files: &Mutex<HashMap<i32,File>>) -> io::Result<i32> {
    // fs_lock.read().unwrap();
    let file = File::open(uri);
    match file {
        Ok(file) => {
            let mut open_files = open_files.lock().unwrap();
            let fd = file.as_raw_fd();
            open_files.insert(fd, file);
            Ok(fd)
        },
        Err(e) => Err(e),
    }
}

pub async fn open_file(file: FileEntry, state: &Arc<DaemonState>) -> Result<i32, VPFSError> {
    if file.owner == state.local.name {
        if let Ok(fd) = open_file_local(&file.uri, &state.open_files) {
            return Ok(fd);
        }
        return Err(VPFSError::DoesNotExist);
    }
    let file_owner_connection = get_connection(&file.owner, state).await;
    if file_owner_connection.is_none() {
        return Err(VPFSError::NotAccessible);
    }
    let file_owner_connection = file_owner_connection.unwrap();
    match file_owner_connection.open_bi().await {
        Ok((mut send, mut recv)) => {
            send_message(&mut send, DaemonRequest::Open(file.uri.clone())).await;
            
            match receive_message(&mut recv).await {
                Ok(DaemonResponse::Open(fd_result)) => {
                    return fd_result;
                },
                Ok(_) => panic!("Bad response"),
                Err(_) => {
                    todo!("Check if error came from bad response, or from connection closing")
                }
            }                
        }
        Err(e) => {
            eprintln!("Error opening bi-directional stream: {}", e);
            return Err(VPFSError::NotAccessible);
        }
        
    }
}

pub fn read_fd_local(fd: i32, len:usize, open_files: &Mutex<HashMap<i32,File>>) -> io::Result<Vec<u8>>{
    let mut open_files = open_files.lock().unwrap();
    let file = open_files
        .get_mut(&fd)
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;

    // let mut reader = BufReader::new(file);
    // let mut buf = Vec::new();

    // reader.take(len as u64)         
    //     .read_to_end(&mut buf)?;

    // Ok(buf)

    let mut buf = vec![0u8; len];
    let n = file.read(&mut buf)?;

    buf.truncate(n);
    Ok(buf)
}

pub fn read_line_fd_local(fd: i32, open_files: &Mutex<HashMap<i32,File>>) -> io::Result<Vec<u8>>{
    let mut open_files = open_files.lock().unwrap();
    let file = open_files
        .get_mut(&fd)
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;

    // let mut reader = BufReader::new(file);
    // let mut line = String::new();

    // reader.read_line(&mut line)?;

    // Ok(line.into_bytes())

    let mut buf = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        let n = file.read(&mut byte)?;
        if n == 0 {
            break; // EOF
        }

        buf.push(byte[0]);
        
        if byte[0] == b'\n' {
            break;
        }
    }

    Ok(buf)
}

pub async fn read_fd(file: &FileEntry, fd:i32, len:usize, state: &Arc<DaemonState>) -> Result<Vec<u8>, VPFSError> {
    if file.owner == state.local.name {
        if let Ok(fd) = read_fd_local(fd, len, &state.open_files) {
            return Ok(fd);
        }
        return Err(VPFSError::FileNotOpen);
    }
    let file_owner_connection = get_connection(&file.owner, state).await;
    if file_owner_connection.is_none() {
        return Err(VPFSError::NotAccessible);
    }
    let file_owner_connection = file_owner_connection.unwrap();
    match file_owner_connection.open_bi().await {
        Ok((mut send, mut recv)) => {
            send_message(&mut send, DaemonRequest::ReadFd(fd, len)).await;
            
            match receive_message(&mut recv).await {
                Ok(DaemonResponse::ReadFd(Ok(()))) => {
                    let buf = receive_message::<Vec<u8>>(&mut recv).await.unwrap();
                    return Ok(buf)
                },
                Ok(DaemonResponse::ReadFd(Err(error))) => {
                    return Err(error)
                },
                Ok(_) => panic!("Bad response"),
                Err(_) => {
                    todo!("Check if error came from bad response, or from connection closing")
                }
            }
        }
        Err(e) => {
            eprintln!("Error opening bi-directional stream: {}", e);
            return Err(VPFSError::NotAccessible);
        }
        
    }
}

pub async fn read_line_fd(file: &FileEntry, fd:i32, state: &Arc<DaemonState>) -> Result<Vec<u8>, VPFSError> {
    if file.owner == state.local.name {
        if let Ok(fd) = read_line_fd_local(fd, &state.open_files) {
            return Ok(fd);
        }
        return Err(VPFSError::FileNotOpen);
    }
    let file_owner_connection = get_connection(&file.owner, state).await;
    if file_owner_connection.is_none() {
        return Err(VPFSError::NotAccessible);
    }
    let file_owner_connection = file_owner_connection.unwrap();
    match file_owner_connection.open_bi().await {
        Ok((mut send, mut recv)) => {
            send_message(&mut send, DaemonRequest::ReadLineFd(fd)).await;
            
            match receive_message(&mut recv).await {
                Ok(DaemonResponse::ReadLineFd(Ok(()))) => {
                    let buf = receive_message::<Vec<u8>>(&mut recv).await.unwrap();
                    return Ok(buf)
                },
                Ok(DaemonResponse::ReadLineFd(Err(error))) => {
                    return Err(error)
                },
                Ok(_) => panic!("Bad response"),
                Err(_) => {
                    todo!("Check if error came from bad response, or from connection closing")
                }
            }
        }
        Err(e) => {
            eprintln!("Error opening bi-directional stream: {}", e);
            return Err(VPFSError::NotAccessible);
        }
        
    }
}

pub fn close_file_local(fd: i32, open_files: &Mutex<HashMap<i32,File>>) -> io::Result<()> {
    let mut open_files = open_files.lock().unwrap();
    if !open_files.contains_key(&fd) {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    open_files.remove(&fd);
    Ok(())
}

pub async fn close_file(node_name: &String, fd: i32, state: &Arc<DaemonState>) -> Result<(), VPFSError> {
    if *node_name == state.local.name {
        if let Ok(()) = close_file_local(fd, &state.open_files) {
            return Ok(());
        }
        return Err(VPFSError::FileNotOpen);
    }
    let file_owner_connection = get_connection(node_name, state).await;
    if file_owner_connection.is_none() {
        return Err(VPFSError::NotAccessible);
    }
    let file_owner_connection = file_owner_connection.unwrap();
    match file_owner_connection.open_bi().await {
        Ok((mut send, mut recv)) => {
            send_message(&mut send, DaemonRequest::Close(fd)).await;
            
            match receive_message(&mut recv).await {
                Ok(DaemonResponse::Close(close_result)) => {
                    return close_result;
                },
                Ok(_) => panic!("Bad response"),
                Err(_) => {
                    todo!("Check if error came from bad response, or from connection closing")
                }
            }                
        }
        Err(e) => {
            eprintln!("Error opening bi-directional stream: {}", e);
            return Err(VPFSError::NotAccessible);
        }
        
    }
}