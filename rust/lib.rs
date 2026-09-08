//! SightOCR core. The UI owns configuration; a single worker owns OCR and HTTP state.
pub mod cli;
pub mod config;
pub mod mcp;
pub mod ocr;
pub mod platform;
pub mod services;
pub mod updater;
pub mod worker;
