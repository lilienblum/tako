pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Doctor should be verbose by default.
    crate::commands::dev::doctor(true).await
}
