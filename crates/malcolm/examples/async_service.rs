//! Async HTTP client wrapped with network fault injection narration.

#[cfg(feature = "tokio")]
use std::time::Duration;

#[cfg(feature = "tokio")]
use malcolm::fault::{Fault, FaultContext};
#[cfg(feature = "tokio")]
use malcolm::faults::network::{LatencySpike, PacketLoss};
#[cfg(feature = "tokio")]
use malcolm_core::bifurcation::BifurcationProfile;

#[cfg(feature = "tokio")]
struct MockHttpClient;

#[cfg(feature = "tokio")]
impl MockHttpClient {
    async fn get_status(&self, path: &str) -> String {
        tokio::time::sleep(Duration::from_millis(10)).await;
        format!("200 OK from {path}")
    }
}

#[cfg(feature = "tokio")]
async fn run() {
    println!("async_service: creating mock client with network faults");

    let profile = BifurcationProfile::network_partition();
    let ctx = FaultContext {
        seed: 88,
        timestamp_ms: 0,
        node_id: "api-gateway".to_owned(),
        profile,
    };

    let latency = LatencySpike::builder()
        .seed(88)
        .base_ms(40.0)
        .sigma(0.4)
        .intensity(0.8)
        .build();
    let packet_loss = PacketLoss::builder()
        .seed(99)
        .alpha(2.0)
        .x_min(1.0)
        .intensity(0.6)
        .build();

    let faults: [&dyn Fault; 2] = [&latency, &packet_loss];
    for fault in faults {
        let result = fault.inject(&ctx);
        println!(
            "async_service: injected={} result={result:?}",
            fault.fault_type()
        );
    }

    let client = MockHttpClient;
    let response = client.get_status("/health").await;
    println!("async_service: response={response}");
}

#[cfg(feature = "tokio")]
fn main() {
    match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime.block_on(run()),
        Err(error) => {
            eprintln!("failed to create tokio runtime: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(feature = "tokio"))]
fn main() {
    println!(
        "async_service example requires the tokio feature. Run: cargo run -p malcolm --example async_service --features tokio"
    );
}
