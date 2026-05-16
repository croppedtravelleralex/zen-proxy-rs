use tracing::info;

pub async fn orchestrator_loop() {
    info!("node probe cycle started");
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}
