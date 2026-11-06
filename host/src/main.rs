use services_native::{Result, Runtime, adapters};
#[tokio::main]
async fn main() {
    match run().await {
        Ok(code) => {
            if code != 0 {
                std::process::exit(code)
            }
        }
        Err(error) => {
            eprintln!("{}", error.message);
            std::process::exit(1);
        }
    }
}
const USAGE: &str = "services-devtools [native-host | --version]\nNo arguments: Chrome native messaging over stdio.";

async fn run() -> Result<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("services-devtools {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Some("--help" | "-h") => {
            eprintln!("{USAGE}");
            return Ok(0);
        }
        None | Some("native-host" | "__native-host-shell") => {}
        Some(other) => {
            eprintln!("Unknown argument: {other}\n{USAGE}");
            return Ok(2);
        }
    }
    #[cfg(unix)]
    if matches!(args.first().map(String::as_str), None | Some("native-host")) {
        return match services_native::shell_environment::launch_native(&args).await {
            Ok(()) => Ok(0),
            Err(error) => {
                eprintln!("{}", error.message);
                adapters::native::reject_startup(&error, tokio::io::stdin(), tokio::io::stdout())
                    .await?;
                Ok(1)
            }
        };
    }
    adapters::native::run(Runtime::new()?, tokio::io::stdin(), tokio::io::stdout()).await?;
    Ok(0)
}
