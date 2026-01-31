use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::os::fd::{AsRawFd, IntoRawFd};
use std::result;
use std::{fs, io::Read};
use std::sync::{Mutex, RwLock};
use std::io::{self, BufRead, BufReader};
use std::sync::Arc;
use rand::Rng;
use lru::LruCache;
use rand::rand_core::le;

use std::sync::MutexGuard;

use crate::{messages::*};

use crate::state::DaemonState;

use crate::remote_communication::*;

/// Create ./files and go to it. Panic if it cannot be created or cd'ed into.
pub fn setup_files_dir() {
    if let Err(err) = fs::create_dir("./files") {
        if err.kind() != std::io::ErrorKind::AlreadyExists {
            panic!("Could not create directory for storing files");
        }
    }
    std::env::set_current_dir("./files").expect("Could not cd into ./files directory");
}

pub fn add_cache_entry(location: &Location, data: &[u8], cache: &mut MutexGuard<LruCache<Location, CacheEntry>>, state: &Arc<DaemonState>) {
    if let Some(cache_entry) = cache.get(&location) {
        fs::write(&cache_entry.uri, &data);
    }
    else {
        let new_cache_entry = CacheEntry {
            uri: create_file_with_random_uri(),
        };
        fs::write(&new_cache_entry.uri, &data);
        cache.put(location.clone(), new_cache_entry);
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
    serde_bare::to_writer(&cache_file, &state.root).expect("Failed to save root node to file");
    serde_bare::to_writer(&cache_file, &*used_cache).expect("Failed to save cahce size to file");
    for (key, value) in cache.iter() {
        serde_bare::to_writer(&cache_file, key).expect("Could not write cache entry to file");
        serde_bare::to_writer(&cache_file, value).expect("Could not write cache entry to file");
    }
}


/// Restore cache from ./cache file if it exists
pub fn restore_cache(state: &mut DaemonState) {
    if let Ok(cache_file) = fs::File::open("cache") {
        let mut cache = state.cache.lock().unwrap();
        state.root = serde_bare::from_reader(&cache_file).expect("Failed to readed from cache file");
        state.used_cache_bytes = serde_bare::from_reader(&cache_file).expect("Failed to readed from cache file");
        while let Ok(key) = serde_bare::from_reader::<_, Location>(&cache_file) {
            let value = serde_bare::from_reader(&cache_file).unwrap();
            cache.put(key.clone(), value);
            cache.demote(&key);
        }
    }
}

pub fn search_directory_with_reader<T: Read>(file_name: &str, directory_reader: &mut T) -> Result<DirectoryEntry, VPFSError> {
    let mut read_result: Result<DirectoryEntry, serde_bare::error::Error> = serde_bare::from_reader(&mut *directory_reader);
    let mut dir_entry = Err(VPFSError::DoesNotExist);
    while let Ok(entry) = read_result {
        if entry.name == file_name{
            dir_entry = Ok(entry);
            break;
        }
        read_result = serde_bare::from_reader(&mut *directory_reader);
    }
    dir_entry
}

//Assumes caller hold file lock
fn search_directory_with_lock(file_name: &str, directory_uri: &str) -> Result<DirectoryEntry, VPFSError> {
    let mut directory_file = fs::File::open(directory_uri).unwrap();
    search_directory_with_reader(file_name, &mut directory_file)
}

fn search_directory(file_name: &str, directory_uri: &str, state: &Arc<DaemonState>) -> Result<DirectoryEntry, VPFSError> {
    let _file_access_lock = state.file_access_lock.read().unwrap();
    search_directory_with_lock(file_name, directory_uri)
}

pub fn append_dir_entry(directory: &str, new_entry: &DirectoryEntry, state: &Arc<DaemonState>) -> Result<(), VPFSError>{
    // Check if the directory entry already exists
    let _fs_lock = state.file_access_lock.write().unwrap();
    if let Ok(existing_dir_entry) = search_directory_with_lock(&new_entry.name, &directory) {
        Err(VPFSError::AlreadyExists(existing_dir_entry))
    }
    else {
        let dir_file = fs::OpenOptions::new().append(true).open(directory).unwrap();
        serde_bare::to_writer(dir_file, &new_entry).unwrap();
        Ok(())
    }
}

pub fn read_local(uri: &str, fs_lock: &RwLock<()>) -> io::Result<Vec<u8>>{
    fs_lock.read().unwrap();
    fs::read(uri)
}

pub fn write_local(uri: &str,  data: &Vec<u8>, fs_lock: &RwLock<()>) -> io::Result<()>{
    fs_lock.write().unwrap();
    if fs::exists(uri)? {
        fs::write(uri, data)
    }
    else {
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

pub async fn read_remote(location: &Location, state: &Arc<DaemonState>) -> Result<Vec<u8>, VPFSError> {
    let mut cache = state.cache.lock().unwrap();
    let cache_entry = cache.get(&location);
    let _fs_lock = state.file_access_lock.write().unwrap();
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
    if let Some(file_owner_connection) = stream_for(&location.node_name, state).await {
        let mut file_owner_connection = file_owner_connection.lock().unwrap();
        match file_owner_connection.open_bi().await {
            Ok((mut send, mut recv)) => {
                send_message(&mut send, DaemonRequest::Read(location.uri.clone(), cache_last_update_time)).await;
                
                match receive_message(&mut recv).await {
                    Ok(DaemonResponse::Read(Ok(()))) => {

                        let buf = receive_message::<Vec<u8>>(&mut recv).await.unwrap();

                        add_cache_entry(location, &buf, &mut cache, state);

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
                eprintln!("✗ Error opening bi-directional stream: {}", e);
                return Err(VPFSError::NotAccessible);
            }
        }
    }
    else {
        if let Some(cache_entry) =  cache_entry{
            let cache_entry_location = Location {
                node_name: state.local.name.clone(),
                uri: cache_entry.uri.clone()
            };
            Err(VPFSError::OnlyInCache(cache_entry_location))
        }
        else {
            Err(VPFSError::NotAccessible)
        }
    }
}

pub async fn place_file(path: &str, at: &String, is_dir: bool, state: &Arc<DaemonState>) -> Result<Location, VPFSError>{
    let uri = if *at == state.local.name {
        create_file_with_random_uri()
    }
    else if let Ok(DaemonResponse::Place(uri)) = send_and_receive(at, DaemonRequest::Place, state).await {
        uri
    }
    else {
        return Err(VPFSError::NotAccessible);
    };
    let new_file_location = Location {
        node_name: at.clone(),
        uri: uri
    };
    let parent_directory_location;
    let file_name;
    if let Some((parent_directory, _file_name)) = path.rsplit_once('/') {
        let parent_directory_entry = recursive_find(parent_directory, state).await?;
        parent_directory_location = parent_directory_entry.location;
        file_name = _file_name;
    } 
    else if let Some(root_node) = &state.root.read().unwrap().clone(){
        parent_directory_location = Location {
            node_name: root_node.name.clone(),
            uri: "root".to_string()
        };
        file_name = path
    }
    else {
        return Err(VPFSError::NotAccessible);
    }
    let mut dir_entry = DirectoryEntry {
        location: new_file_location.clone(),
        name: file_name.to_string(),
        is_dir: is_dir
    };

    let success= if parent_directory_location.node_name == state.local.name {
        append_dir_entry(&parent_directory_location.uri, &dir_entry, state)
    }
    else {
        match send_and_receive(&parent_directory_location.node_name, DaemonRequest::AppendDirectoryEntry(parent_directory_location.uri.clone(), dir_entry.clone()), state).await {
            Ok(DaemonResponse::AppendDirectoryEntry(result)) => result,
            Ok(_) => Err(VPFSError::Other("Bad response".to_string())),
            Err(error) => Err(VPFSError::Other("Connection closed".to_string()))
        }
    };
    // Add . and .. directory entries if new file is a directory
    if success.is_ok() && is_dir {
        let dot_dot_entry = DirectoryEntry {
            location: parent_directory_location.clone(),
            name: "..".to_string(),
            is_dir: true,
        };
        dir_entry.name = ".".to_string();
        if *at == state.local.name {
            let _ = append_dir_entry(&new_file_location.uri, &dir_entry, state);
            let _ = append_dir_entry(&new_file_location.uri, &dot_dot_entry, state);
        }
        else {
            send_and_receive::<_, DaemonResponse>(at, DaemonRequest::AppendDirectoryEntry(new_file_location.uri.clone(), dir_entry), state).await;
            send_and_receive::<_, DaemonResponse>(at, DaemonRequest::AppendDirectoryEntry(new_file_location.uri.clone(), dot_dot_entry), state).await;
        }
    }
    else if let Err(error) = success {
        if *at == state.local.name {
            fs::remove_file(&new_file_location.uri);
        }
        else {
            send_and_receive::<_, DaemonResponse>(at, DaemonRequest::Remove(new_file_location.uri), state).await;
        }
        return Err(error);
    }
    
    Ok(new_file_location)
}


pub async fn recursive_find(file: &str, state: &Arc<DaemonState>) -> Result<DirectoryEntry, VPFSError> {
    if let Some((parent_directory, file_name)) = file.rsplit_once('/') {
        match Box::pin(recursive_find(parent_directory, state)).await {
            Ok(parent_dir_entry) => {
                if !parent_dir_entry.is_dir {
                    return Err(VPFSError::NotADirectory);
                }
                
                if parent_dir_entry.location.node_name == state.local.name {
                    search_directory(file_name, &parent_dir_entry.location.uri, state)
                }
                else {
                    match read_remote(&parent_dir_entry.location, state).await {
                        Ok(directory) => search_directory_with_reader(file_name, &mut BufReader::new(&*directory)),
                        Err(VPFSError::OnlyInCache(cache_location)) => {
                            let dir_entry = search_directory(file_name, &cache_location.uri, state);
                            if let Ok(dir_entry) = dir_entry {
                                Err(VPFSError::CacheNeededForTraversal(dir_entry))
                            } else {
                                dir_entry
                            }
                        },
                        Err(error) => Err(error)
                    }
                }
            },
            Err(VPFSError::CacheNeededForTraversal(parent_dir_entry)) => {
                if !parent_dir_entry.is_dir {
                    return Err(VPFSError::NotADirectory);
                }
                if parent_dir_entry.location.node_name == state.local.name {
                    let dir_entry = search_directory(file_name, &parent_dir_entry.location.uri, state);
                    if let Ok(dir_entry) = dir_entry {
                        Err(VPFSError::CacheNeededForTraversal(dir_entry))
                    } else {
                        dir_entry
                    }
                }
                else {
                    match read_remote(&parent_dir_entry.location, state).await {
                        Ok(directory) => {
                            let dir_entry = search_directory_with_reader(file_name, &mut BufReader::new(&*directory));
                            if let Ok(dir_entry) = dir_entry {
                                Err(VPFSError::CacheNeededForTraversal(dir_entry))
                            } else {
                                dir_entry
                            }
                        },
                        Err(VPFSError::OnlyInCache(cache_location)) => {
                            let dir_entry = search_directory(file_name, &cache_location.uri, state);
                            if let Ok(dir_entry) = dir_entry {
                                Err(VPFSError::CacheNeededForTraversal(dir_entry))
                            } else {
                                dir_entry
                            }
                        },
                        Err(error) => Err(error)
                    }
                }
            }
            error => error
        }
    }
    else if let Some(root_node) = state.root.read().unwrap().as_ref() {
        if *root_node == state.local {
            search_directory(file, "root", state)
        }
        else {
            let root_location = Location {
                node_name: root_node.name.clone(),
                uri: "root".to_string()
            };
            match read_remote(&root_location, state).await {
                Ok(root_dir) => search_directory_with_reader(file, &mut BufReader::new(&*root_dir)),
                Err(VPFSError::OnlyInCache(cache_location)) => {
                    let dir_entry = search_directory(file, &cache_location.uri, state);
                    if let Ok(dir_entry) = dir_entry {
                        Err(VPFSError::CacheNeededForTraversal(dir_entry))
                    } else {
                        dir_entry
                    }
                },
                Err(error) => Err(error)
            }
        }
    }
    else {
        Err(VPFSError::NotAccessible)
    }
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

pub async fn open_file(location: Location, state: &Arc<DaemonState>) -> Result<i32, VPFSError> {
    if location.node_name == state.local.name {
        if let Ok(fd) = open_file_local(&location.uri, &state.open_files) {
            return Ok(fd);
        }
        return Err(VPFSError::DoesNotExist);
    }
    let file_owner_connection = stream_for(&location.node_name, state).await;
    if file_owner_connection.is_none() {
        return Err(VPFSError::NotAccessible);
    }
    let file_owner_connection = file_owner_connection.unwrap();
    let mut file_owner_connection = file_owner_connection.lock().unwrap();
    match file_owner_connection.open_bi().await {
        Ok((mut send, mut recv)) => {
            send_message(&mut send, DaemonRequest::Open(location.uri.clone())).await;
            
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
            eprintln!("✗ Error opening bi-directional stream: {}", e);
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

pub async fn read_fd(location: &Location, fd:i32, len:usize, state: &Arc<DaemonState>) -> Result<Vec<u8>, VPFSError> {
    if location.node_name == state.local.name {
        if let Ok(fd) = read_fd_local(fd, len, &state.open_files) {
            return Ok(fd);
        }
        return Err(VPFSError::FileNotOpen);
    }
    let file_owner_connection = stream_for(&location.node_name, state).await;
    if file_owner_connection.is_none() {
        return Err(VPFSError::NotAccessible);
    }
    let file_owner_connection = file_owner_connection.unwrap();
    let mut file_owner_connection = file_owner_connection.lock().unwrap();
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
            eprintln!("✗ Error opening bi-directional stream: {}", e);
            return Err(VPFSError::NotAccessible);
        }
        
    }
}

pub async fn read_line_fd(location: &Location, fd:i32, state: &Arc<DaemonState>) -> Result<Vec<u8>, VPFSError> {
    if location.node_name == state.local.name {
        if let Ok(fd) = read_line_fd_local(fd, &state.open_files) {
            return Ok(fd);
        }
        return Err(VPFSError::FileNotOpen);
    }
    let file_owner_connection = stream_for(&location.node_name, state).await;
    if file_owner_connection.is_none() {
        return Err(VPFSError::NotAccessible);
    }
    let file_owner_connection = file_owner_connection.unwrap();
    let mut file_owner_connection = file_owner_connection.lock().unwrap();
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
            eprintln!("✗ Error opening bi-directional stream: {}", e);
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
    let file_owner_connection = stream_for(node_name, state).await;
    if file_owner_connection.is_none() {
        return Err(VPFSError::NotAccessible);
    }
    let file_owner_connection = file_owner_connection.unwrap();
    let mut file_owner_connection = file_owner_connection.lock().unwrap();
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
            eprintln!("✗ Error opening bi-directional stream: {}", e);
            return Err(VPFSError::NotAccessible);
        }
        
    }
}