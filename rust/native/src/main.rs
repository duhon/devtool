use services_native::{Error, Result, Runtime, adapters};
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
        Some("web") => {
            let mut port = std::env::var("PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(4569);
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--port" {
                    i += 1;
                    port = args
                        .get(i)
                        .ok_or_else(|| Error::new("--port requires a value"))?
                        .parse()
                        .map_err(|_| Error::new("Invalid port"))?;
                } else if let Some(value) = args[i].strip_prefix("--port=") {
                    port = value.parse().map_err(|_| Error::new("Invalid port"))?;
                } else {
                    return Err(Error::new(format!("Unknown web argument: {}", args[i])));
                }
                i += 1;
            }
            adapters::web::run(runtime, port).await?;
            Ok(0)
        }
        Some("--help") | Some("-h") => {
            eprintln!(
                "services-devtools [native-host | cli <service> [options] | web [--port N] | --version]\nNo arguments: Chrome native messaging over stdio.\n{}",
                adapters::cli::usage(None)
            );
            Ok(0)
        }
        _ => adapters::cli::run(runtime, &args).await,
    }
}
