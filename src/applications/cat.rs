use clap::{Parser, Subcommand};

use std::sync::Arc;
use std::io::{self, Write};

use vpfs::*;
use vpfs::messages::*;

#[derive(Parser, Debug)]
#[command(name = "cat", about = "VPFS cat utility")]
struct Opt {
    #[arg(short, long, default_value_t = 8080)]
    port: u16,

    pub path: String,
}

fn cat(vpfs: &VPFS, path: &str) -> Result<(), VPFSError> {
    let data = vpfs.fetch(path)?;
    io::stdout().write_all(&data).unwrap();
    Ok(())
}

fn main() {
    let opt = Opt::parse();
    let vpfs = Arc::new(VPFS::connect(opt.port).expect("Failed to connect to local daemon"));
    let mut cwd = "".to_string(); // TODO

    let data = vpfs.fetch(&opt.path)
        .expect("Failed to read file");

    io::stdout().write_all(&data).unwrap();



}