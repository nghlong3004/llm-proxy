//! Binary entrypoint; CLI parsing lives in the inbound adapter, composition in bootstrap.
#[tokio::main]
async fn main() {
    if let Err(error) = llm_proxy::adapters::inbound::cli::run().await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
