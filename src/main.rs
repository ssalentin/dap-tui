mod app;
mod config;
mod disk;
mod engine;
mod progress;
mod udisks;
mod ui;

use anyhow::Result;

fn print_help() {
    println!("dap-tui — device sync TUI (ratatui)");
    println!();
    println!("Usage: dap-tui [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --config <PATH>    use an alternate config file");
    println!("  -s, --skip-mirror  skip the StorageBox → local mirror step");
    println!("  -V, --version      print version");
    println!("  -h, --help         show this help");
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut config_path: Option<String> = None;
    let mut skip_mirror = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("dap-tui {}", app::VERSION);
                return Ok(());
            }
            "--config" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    config_path = Some(p.clone());
                }
            }
            "-s" | "--skip-mirror" => skip_mirror = true,
            other if other.starts_with("--config=") => {
                config_path = Some(other[9..].to_string());
            }
            _ => {}
        }
        i += 1;
    }

    let cfg = config::load(config_path)?;
    let mut app = app::App::new(cfg, skip_mirror)?;
    app.run()
}
