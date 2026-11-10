//! Integration tests, one binary: each module was its own test target
//! (fewer binaries to link, and for macOS to scan, after each build).

mod badges;
mod config;
mod demo;
mod diff;
mod discover;
mod fix;
mod gitignore;
mod icons;
mod kept;
mod literals;
mod properties;
mod pull;
mod roots;
mod stash;
mod status;
mod workspace;
