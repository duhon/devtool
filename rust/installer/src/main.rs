use anyhow::{Context, Result, bail};
use std::path::PathBuf;

fn run() -> Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|arg| arg.to_str()) {
        Some("install") if args.len() == 2 => {
            services_installer::install(&home, &PathBuf::from(&args[1]))?;
        }
        Some("uninstall") if args.len() == 1 => {
            services_installer::uninstall(&home)?;
        }
        Some("update") if args.len() == 4 => {
            services_installer::updates::install_archive(
                &home,
                &PathBuf::from(&args[1]),
                args[2].to_str().context("Invalid update checksum")?,
                args[3].to_str().context("Invalid update version")?,
            )?;
        }
        _ => bail!(
            "Usage: services-install install PAYLOAD | update ARCHIVE SHA256 VERSION | uninstall"
        ),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Installation failed: {error:#}");
        std::process::exit(1);
    }
}
