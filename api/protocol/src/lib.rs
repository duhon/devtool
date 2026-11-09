pub mod agent;
pub mod native_messaging;
pub mod package;
pub mod templates;
pub mod updates;

pub const PROTOCOL_VERSION: u32 = 1;
/// A pre-release build (`xtask build --prerelease` sets this variable) installs beside the
/// release, so it has its own native host and installation folder.
pub const PRERELEASE: bool = option_env!("SERVICES_DEVTOOLS_PRERELEASE").is_some();
pub const HOST_NAME: &str = if PRERELEASE {
    "com.chrome_extentions.host.prerelease"
} else {
    "com.chrome_extentions.host"
};
pub const APPLICATION: &str = if PRERELEASE {
    "ServicesDevTools-Prerelease"
} else {
    "ServicesDevTools"
};
