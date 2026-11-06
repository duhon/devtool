//! CONTRACT: the transports every module is reached through: native
//! messaging, the CLI binary, the login-shell environment and TLS setup.

mod drivers;
mod native;
mod packages;
mod shell_environment;
mod tls;
