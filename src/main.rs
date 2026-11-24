use cloudnest::cli;

#[tokio::main]
async fn main() {
    if let Err(err) = cli::run().await {
        eprintln!("cloudnest: {err}");
        std::process::exit(1);
    }
}
