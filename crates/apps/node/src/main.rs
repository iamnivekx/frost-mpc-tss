use clap::Parser;
use tracing::error;

#[tokio::main]
async fn main() {
    let command = mpc_node::Command::parse();
    if let Err(err) = command.execute().await {
        error!("Error: {err:?}");
        std::process::exit(1);
    }
}
