//! Integration tests, one binary: each module was its own test target
//! (fewer binaries to link, and for macOS to scan, after each build).

mod app;
mod properties;
mod runtime;
