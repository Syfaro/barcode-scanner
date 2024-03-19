mod barcode_decoders;
mod barcode_scanner;
mod config;
mod ui;

fn main() {
    tracing_subscriber::fmt()
        .pretty()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("hello! :3");

    // Safety: Environment variables aren't mutated, so this should be safe.
    unsafe {
        time::util::local_offset::set_soundness(time::util::local_offset::Soundness::Unsound)
    };

    ui::show_ui().expect("could not display ui");
}
