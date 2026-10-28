//! Terminal presentation shared by the full-screen chat and the command line.

pub mod ansi;
pub mod highlight;
pub mod markdown;
pub mod text;
pub mod theme;

pub use theme::{Role, theme};
