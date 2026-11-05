//! `sill-mockhost [port]`: a mock host on loopback for trying the client by hand.
use sill_mockhost::{MockConfig, MockHost};

#[tokio::main]
async fn main() {
    let host = MockHost::start(MockConfig::default()).await.expect("bind");
    println!("sill-mockhost listening on {} (pin {})", host.addr, sill_net::pin::hex(&host.fingerprint));
    tokio::signal::ctrl_c().await.ok();
}
