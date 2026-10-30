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
async fn run() -> Result<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--version" || s == "-V") {
        println!("services-devtools {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
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
    let runtime = Runtime::new()?;
    match args.first().map(String::as_str) {
        None | Some("native-host" | "__native-host-shell") => {
            adapters::native::run(runtime, tokio::io::stdin(), tokio::io::stdout()).await?;
            Ok(0)
        }
        Some("cli") => adapters::cli::run(runtime, &args[1..]).await,
        Some("--help") | Some("-h") => {
            eprintln!(
                "services-devtools [native-host | cli <service> [options] | --version]\nNo arguments: Chrome native messaging over stdio.\n{}",
                adapters::cli::usage(runtime.registry.manifests().iter())
            );
            Ok(0)
        }
        _ => adapters::cli::run(runtime, &args).await,
    }
}
