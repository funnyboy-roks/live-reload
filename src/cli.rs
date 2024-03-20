use std::{net::IpAddr, path::PathBuf};

use clap::Parser;

#[derive(Debug, Parser)]
pub struct Cli {
    /// Port on which to listen for requests
    #[arg(short, long, default_value = "4000")]
    pub port: u16,
    /// Address on which to listen for requests
    #[arg(short, long, default_value = "0.0.0.0")]
    pub addr: IpAddr,
    /// Run the server as a static http server, rather than injecting the JavaScript to allow the
    /// page to be hot-reloaded (this also disables listening for SIGHUP and the websocket api).
    #[arg(short = 's', long = "static")]
    pub static_only: bool,
    pub directory: PathBuf,
}
