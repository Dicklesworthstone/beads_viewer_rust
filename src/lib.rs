#![forbid(unsafe_code)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::implicit_hasher)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::too_many_lines)]

pub mod agents;
pub mod analysis;
pub mod cli;
pub mod error;
pub mod export_md;
pub mod export_pages;
pub mod export_sqlite;
pub mod loader;
pub mod model;
pub mod pages_wizard;
pub mod robot;
pub mod tui;
pub mod viewer_assets;

pub use error::{BvrError, Result};
