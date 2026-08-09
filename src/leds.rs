//! LED service thread for dc34-leds.
//!
//! Spawns a thread that owns the BIO `LedDriver` (pin 15, 10 LEDs) and
//! listens on its own private server for `Next`/`Prev` scalar messages sent
//! by the main thread on button presses. No PDDB persistence.

use arbitrary_int::u5;
use num_derive::FromPrimitive;

use crate::bio::{LedDriver, PatternKind};

/// LED control server name.
///
/// This MUST be `_oem_led_` (the same name `dc34_api::LED_SERVER` used): the
/// BIO co-processor server in `bao1x-hal-service` does
/// `request_connection_blocking("_oem_led_")` at startup when built with the
/// `oem-baosec-lite` feature. If nobody registers this name, the BIO server
/// blocks forever, never registers `_BIO server_`, and our `Bio::new()`
/// (inside `LedDriver::new`) hangs, leaving the LEDs dark. By registering the
/// LED control server under `_oem_led_` we unblock the BIO server so it can
/// come up and service our driver.
pub const LEDS_CTL_SERVER: &str = "_oem_led_";

/// Opcodes accepted by the LED control server.
#[derive(Debug, FromPrimitive)]
#[repr(usize)]
pub enum LedCtlOp {
    /// Advance to the next pattern in the ordered list.
    Next = 0,
    /// Go back to the previous pattern in the ordered list.
    Prev = 1,
    Invalid = 2,
    /// Blocking scalar sent by the BIO server on `BioOp::PrepFreqChange`
    /// (clock transitions) to pause rendering while the clock changes. We
    /// must reply so the blocking caller unblocks; we otherwise do nothing.
    /// See `bao1x-hal-service/src/servers/bio.rs:136-140`, which sends
    /// `Message::new_blocking_scalar(128, ...)`. This is a fixed protocol
    /// opcode and must not collide with `Next`/`Prev`/`Invalid`.
    Pause = 128,
}

/// Number of onboard WS2812 LEDs on the badge.
const LED_COUNT: u8 = 10;
/// GPIO pin the WS2812 strip is wired to.
const LED_PIN: u8 = 15;

/// Spawn the LED service thread.
pub fn start_leds() {
    std::thread::spawn(move || {
        leds();
    });
}

fn leds() {
    let xns = xous_names::XousNames::new().unwrap();
    let sid = xns.register_name(LEDS_CTL_SERVER, None).unwrap();

    let initial_pattern = PatternKind::BrRunner;
    log::info!("starting LED service with pattern {:?}", initial_pattern);

    let mut driver = LedDriver::new(u5::new(LED_PIN), LED_COUNT, None, initial_pattern)
        .expect("couldn't init BIO LED driver");
    log::info!("LedDriver::new OK, pattern running");

    let mut msg_opt = None;
    loop {
        xous::reply_and_receive_next(sid, &mut msg_opt).unwrap();
        let opcode = {
            let msg = msg_opt.as_mut().unwrap();
            num_traits::FromPrimitive::from_usize(msg.body.id()).unwrap_or(LedCtlOp::Invalid)
        };
        match opcode {
            LedCtlOp::Next => {
                if let Err(e) = driver.next_pattern() {
                    log::error!("next pattern failed: {:?}", e);
                }
            }
            LedCtlOp::Prev => {
                if let Err(e) = driver.prev_pattern() {
                    log::error!("prev pattern failed: {:?}", e);
                }
            }
            LedCtlOp::Pause => {
                // The BIO server sends this as a BLOCKING scalar during clock
                // transitions (PrepFreqChange). We do not need to do anything
                // to the driver here; we only MUST reply so the blocking caller
                // (the BIO server) unblocks and does not deadlock. The reply is
                // sent automatically by the next `reply_and_receive_next()`, but
                // we return_scalar explicitly so the BIO server unblocks
                // immediately rather than waiting for the next message.
                let sender = msg_opt.as_ref().unwrap().sender;
                xous::return_scalar(sender, 0).ok();
                // We've replied manually; clear msg_opt so the loop's
                // reply_and_receive_next does not try to auto-reply again.
                msg_opt = None;
            }
            LedCtlOp::Invalid => {
                log::error!("Invalid LED control operation");
            }
        }
    }
}
