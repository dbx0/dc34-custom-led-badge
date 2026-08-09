//! Slim BIO WS2812 LED driver for the dc34-leds badge app.
//!
//! This is a stripped-down version of the `Lightgenes` renderer from
//! dc34-console. All gene / meiosis / express / PDDB machinery has been
//! removed; this driver only knows how to load one of a small set of
//! self-contained animation programs onto the BIO co-processor and drive
//! the badge's onboard WS2812 strip.

pub mod lightgenes;

pub use lightgenes::{LedDriver, PatternKind};
