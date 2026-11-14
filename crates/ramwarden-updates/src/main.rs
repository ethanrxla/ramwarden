#![cfg_attr(windows, windows_subsystem = "windows")]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "check".into());
    match command.as_str() {
        "check" => println!(
            "{}",
            serde_json::to_string(&ramwarden_updates::check().await).unwrap()
        ),
        "download" | "update" => match ramwarden_updates::download().await {
            Ok(path) => {
                if command == "update"
                    && let Err(error) = ramwarden_updates::open(&path)
                {
                    eprintln!("{error}");
                    std::process::exit(1)
                }
                println!("{}", serde_json::json!({"installer":path,"verified":true}));
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1)
            }
        },
        _ => {
            eprintln!("Use ramwarden-launcher check, download or update");
            std::process::exit(2)
        }
    }
}
