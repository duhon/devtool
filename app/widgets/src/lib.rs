//! Generic UI components: no application logic, no Kernel. Each widget carries its CSS
//! (`CSS`), coloured only through the theme's variables the Shell defines.
pub mod editor;
pub mod find_bar;
pub mod indicator;
pub mod status_bar;

#[cfg(target_arch = "wasm32")]
pub mod check_rail;
#[cfg(target_arch = "wasm32")]
pub mod dom;
#[cfg(target_arch = "wasm32")]
pub mod markdown;
#[cfg(target_arch = "wasm32")]
pub mod resizer;
#[cfg(target_arch = "wasm32")]
pub mod tree;
#[cfg(target_arch = "wasm32")]
pub mod view;
