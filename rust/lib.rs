//! SightOCR core. The UI owns configuration; a single worker owns OCR and HTTP state.
pub mod config;
mod headless;
pub mod mcp;
pub mod ocr;
pub mod platform;
pub mod services;
pub mod updater;
pub mod worker;
