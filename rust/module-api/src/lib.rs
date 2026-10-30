//! Contract between Services DevTools and its service modules.
//!
//! - [`transport`]: the ABI — gRPC-like unary / streaming calls across the dylib boundary.
//! - [`proto`]: messages of `proto/host.proto` (host functions) and `proto/module.proto` (module methods).
//! - [`sdk`]: what a module implements ([`sdk::Service`]) and how it calls the host ([`sdk::Host`]).
//! - [`client`]: how the host calls a module.

pub use prost;
pub use stabby;

pub mod client;
pub mod error;
pub mod http;
pub mod proto;
pub mod sdk;
pub mod transport;

/// Module ABI version `major.minor`: a breaking change bumps the major together with [`ENTRY`];
/// a module needs a host whose minor is at least its own.
pub const ABI_VERSION: &str = "3.0";
#[doc(hidden)]
pub const ABI_VERSION_C: &core::ffi::CStr = c"3.0";
/// Exported plain-C symbol returning the [`ABI_VERSION`] a module was built for. Its signature
/// never changes, so the host reads it before anything else and explains a mismatch.
pub const ABI_SYMBOL: &[u8] = b"services_module_abi";
/// Exported entry symbol of a module library (stabby-checked).
pub const ENTRY: &[u8] = b"services_module_v3";

/// The entry a module library exports as [`ENTRY`].
pub type Entry = extern "C" fn(
    transport::EndpointArc,
)
    -> stabby::result::Result<transport::EndpointBox, transport::Status>;

/// Exports a module library: `export!(endpoint)` where `endpoint` builds the module's endpoint
/// (usually [`sdk::serve`]). The crate must be a `cdylib` and depend on `stabby` directly.
#[macro_export]
macro_rules! export {
    ($endpoint:path) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn services_module_abi() -> *const ::core::ffi::c_char {
            $crate::ABI_VERSION_C.as_ptr()
        }

        #[stabby::export]
        pub extern "C" fn services_module_v3(
            host: $crate::transport::EndpointArc,
        ) -> stabby::result::Result<$crate::transport::EndpointBox, $crate::transport::Status> {
            $crate::transport::catch(|| $endpoint(host))
        }
    };
}
