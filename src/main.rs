//! Binary entrypoint; CLI parsing and command wiring live in cli/.
#[tokio::main]
async fn main() {
    if let Err(error) = llm_proxy::cli::run().await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
