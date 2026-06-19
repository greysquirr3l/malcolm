//! Async HTTP client wrapped with network fault injection narration.

#[cfg(feature = "tokio")]
mod runtime {
    use std::time::Duration;

    use malcolm::fault::{Fault, FaultContext};
    use malcolm::faults::network::{LatencySpike, PacketLoss};
    use malcolm_core::bifurcation::BifurcationProfile;

    struct MockHttpClient;

    impl MockHttpClient {
        async fn get_status(&self, path: &str) -> String {
            tokio::time::sleep(Duration::from_millis(10)).await;
            format!("200 OK from {path}")
        }
    }

    pub(crate) async fn run() {
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
}

#[cfg(feature = "tokio")]
fn main() {
    match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime.block_on(runtime::run()),
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
