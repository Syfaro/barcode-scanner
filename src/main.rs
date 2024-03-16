mod barcode_scanner;
mod ui;

fn main() {
    tracing_subscriber::fmt()
        .pretty()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("hello! :3");

    ui::show_ui().expect("could not display ui");
}
