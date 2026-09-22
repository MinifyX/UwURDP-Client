//! A toy RDP server for manual tests and the app's end-to-end run.
//!
//! ```text
//! cargo run --example dev_rdpd
//! UWURDP_DEV_RDPD_ADDR=0.0.0.0:3390 cargo run --example dev_rdpd
//! ```
//!
//! Log in as `uwu` / `nyu` (NLA). A fresh self-signed certificate is made on
//! every start; its fingerprint is printed so it can be compared with what
//! the app shows in its trust dialog.

// The dev-dependency ironrdp-server links aws-lc, whose objects export a few
// symbols; MSVC then reports creating an import library. Harmless, and it
// never happens in the app, which does not link aws-lc.
#![allow(linker_messages)]

#[path = "../tests/support/dev_server.rs"]
mod dev_server;

use std::net::SocketAddr;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr: SocketAddr = std::env::var("UWURDP_DEV_RDPD_ADDR")
        .unwrap_or_else(|_| dev_server::DEFAULT_ADDR.to_owned())
        .parse()?;

    let server = dev_server::start(dev_server::DevServerOptions {
        addr,
        ..Default::default()
    })?;

    println!("dev_rdpd listening on {}", server.addr);
    println!("certificate fingerprint: {}", server.fingerprint);
    println!(
        "credentials: {} / {} (NLA)",
        dev_server::USERNAME,
        dev_server::PASSWORD
    );
    println!("press Ctrl+C to stop");

    // The server runs on its own thread; this one only has to stay alive.
    loop {
        std::thread::park();
    }
}
