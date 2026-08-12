mod background;
mod bio;
mod leds;
mod nyan;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use num_derive::FromPrimitive;
use ux_api::service::gfx::Gfx;

use crate::bio::PatternKind;
use crate::leds::{LedCtlOp, LEDS_CTL_SERVER};

/// Server name this app registers so the gfx subsystem can route filtered
/// keyboard events to us.
const SERVER_NAME_LEDS: &str = "_dc34_leds_";

/// Opcodes handled by our main server loop.
#[derive(Debug, FromPrimitive)]
#[repr(usize)]
enum LedAppOp {
    /// Filtered keyboard event delivered by the gfx subsystem.
    KeyPress = 0,
    /// Periodic self-poke to redraw the background so it persists.
    Redraw = 1,
    Invalid = 2,
}

/// Draw the background bitmap as the full-screen background.
///
/// bao-video silently *drops* Clear/Flush (returning Ok either way) whenever it
/// is in `dry_run` mode or has a pending `qr_request` (see
/// xous-core/services/bao-video/src/main.rs: the `GfxOpcode::Clear` and
/// `GfxOpcode::Flush` arms are gated on `if qr_request.is_none()` / `if
/// !dry_run`). The BaosecBitmap arm, by contrast, is *ungated* and always
/// writes into the framebuffer. That asymmetry is exactly the failure mode for
/// a persistent white screen: `bitmap()` populates the buffer but the `flush()`
/// that would push it to the panel is quietly discarded, so the display stays
/// at the all-`0xFFFF_FFFF` (white) state that bao-video sets at init
/// (display.init() -> clear() -> draw()).
///
/// Since `.ok()` hides that (the calls "succeed"), we defensively clear any
/// stuck `dry_run` state before drawing, capture and log the real `Result` of
/// each step, and log a screen_size() probe (a *blocking* round-trip that
/// proves the gfx server is actually servicing us).
fn draw_background(gfx: &Gfx) {
    draw_frame(gfx, &background::BITMAP);
}

/// Draw an arbitrary 128x128 `[u32; 512]` frame to the panel. Shares the same
/// defensive dry_run/clear/bitmap/flush sequence as draw_background (see its
/// doc comment for why dry_run is cleared first).
fn draw_frame(gfx: &Gfx, frame: &[u32]) {
    if let Err(e) = gfx.dry_run(false) {
        log::warn!("gfx.dry_run(false) err: {:?}", e);
    }
    if let Err(e) = gfx.clear() {
        log::warn!("gfx.clear err: {:?}", e);
    }
    if let Err(e) = gfx.bitmap(frame, None, None) {
        log::warn!("gfx.bitmap err: {:?}", e);
    }
    if let Err(e) = gfx.flush() {
        log::warn!("gfx.flush err: {:?}", e);
    }
}

/// Ask the xous log server to mirror all console/log output to the USB serial
/// port so a host reading /dev/cu.usbmodem* can see our logs. The USB serial
/// device must already be set up (UsbHid::new + serial_console_input_injection)
/// before this is called.
///
/// This sends the well-known blocking scalar opcode 4 (TryHookUsbMirror) to the
/// log server, which is registered under the SID `b"xous-log-server "`. The same
/// hook is performed by usb-bao1x itself in its SerialHookConsole handler
/// (xous-core/services/usb-bao1x/src/main.rs:687), which is our reference.
fn hook_usb_log_mirror() {
    let log_conn = match xous::connect(xous::SID::from_bytes(b"xous-log-server ").unwrap()) {
        Ok(c) => c,
        Err(e) => {
            log::error!("could not connect to log server for USB mirror: {:?}", e);
            return;
        }
    };
    // The log server replies with Scalar1(1) on success. Retry a few times in
    // case USB is not quite ready yet.
    for attempt in 0..5 {
        match xous::send_message(
            log_conn,
            xous::Message::new_blocking_scalar(
                4, /* log_server::api::Opcode::TryHookUsbMirror */
                0, 0, 0, 0,
            ),
        ) {
            Ok(xous::Result::Scalar1(1)) => {
                log::info!("USB log mirror hooked (attempt {})", attempt);
                return;
            }
            Ok(other) => {
                log::warn!("USB log mirror hook returned {:?} (attempt {})", other, attempt);
            }
            Err(e) => {
                log::warn!("USB log mirror hook send failed: {:?} (attempt {})", e, attempt);
            }
        }
        ticktimer::Ticktimer::new().unwrap().sleep_ms(500).ok();
    }
    log::error!("USB log mirror hook did not succeed after retries");
}

fn main() {
    log_server::init_wait().unwrap();
    log::set_max_level(log::LevelFilter::Info);
    log::info!("dc34-leds starting, PID {}", xous::process::id());

    // Feed the hardware watchdog. A WDT is left running by the boot stage and
    // will reset the SoC if not periodically fed. The stock firmware fed it from
    // its power manager; we replicate just the feeding here (we do NOT arm/enable
    // it). Feeding a stopped WDT is harmless, so this is safe regardless. Start
    // this as early as possible so the dog is fed before the gfx bring-up wait.
    std::thread::spawn(|| {
        let mut wdt = bao1x_hal::wdt::Wdt::new();
        let tt = ticktimer::Ticktimer::new().unwrap();
        log::info!("wdt feeder started");
        loop {
            wdt.feed();
            tt.sleep_ms(2000).ok();
        }
    });

    // Bring up the USB serial device and enable console input injection, then
    // ask the log server to mirror all log output to USB so a host reading
    // /dev/cu.usbmodem* can observe our logs (the DUART is not host-readable).
    let usb = usb_bao1x::UsbHid::new();
    usb.serial_console_input_injection();
    // Give USB a moment to come up before requesting the mirror hook.
    ticktimer::Ticktimer::new().unwrap().sleep_ms(500).ok();
    hook_usb_log_mirror();
    log::info!("=== dc34-leds boot: usb+logmirror up ===");

    // Register a server name so this app is a "real" registered app like the
    // vault, and so the gfx subsystem can route keyboard events to us.
    let xns = xous_names::XousNames::new().unwrap();
    let sid = xns.register_name(SERVER_NAME_LEDS, None).unwrap();
    let self_conn = xous::connect(sid).unwrap();
    log::info!("dc34-leds registered server {}, sid {:?}", SERVER_NAME_LEDS, sid);

    let tt = ticktimer::Ticktimer::new().unwrap();

    // Give the gfx server (bao-video) time to finish display init before the
    // first draw; a one-shot draw at t=0 races startup and gets left as the
    // white default screen.
    tt.sleep_ms(1500).ok();

    log::info!("about to Gfx::new");
    let gfx = Gfx::new(&xns).unwrap();
    log::info!("Gfx::new ok");
    log::info!("dc34-leds gfx connected");

    // Probe the gfx server with a *blocking* round-trip. If this returns, the
    // server is up and servicing our connection; the reported size also lets us
    // sanity-check the panel geometry the bitmap is being drawn into.
    match std::panic::catch_unwind(|| gfx.screen_size()) {
        Ok(Ok(sz)) => log::info!("gfx.screen_size ok: {}x{}", sz.x, sz.y),
        Ok(Err(e)) => log::warn!("gfx.screen_size err: {:?}", e),
        Err(_) => log::error!("gfx.screen_size panicked (gfx server not servicing?)"),
    }

    // Draw the badge background.
    log::info!("initial draw_background");
    draw_background(&gfx);
    log::info!("initial draw_background done");

    // Route filtered keyboard events to our server.
    gfx.register_listener(SERVER_NAME_LEDS, LedAppOp::KeyPress as usize);

    // Start the LED animation service and connect to its control server.
    log::info!("about to start_leds");
    leds::start_leds();
    // The LED thread registers its server on startup; give it a moment, then
    // grab a connection. request_connection_blocking waits for registration.
    let led_ctl = xns.request_connection_blocking(LEDS_CTL_SERVER).unwrap();
    log::info!("led_ctl connected");
    log::info!("dc34-leds connected to LED control server");

    // Spawn a low-frequency redraw ticker so the background survives any
    // re-blit from other servers. It just pokes our own server.
    std::thread::spawn(move || {
        let tt = ticktimer::Ticktimer::new().unwrap();
        loop {
            tt.sleep_ms(2000).ok();
            xous::try_send_message(
                self_conn,
                xous::Message::new_scalar(LedAppOp::Redraw as usize, 0, 0, 0, 0),
            )
            .ok();
        }
    });

    // ---- Easter egg: nyan animation ------------------------------------------
    // When `nyan_active` is set, a dedicated player thread cycles the baked
    // nyan.gif frames (~10 fps) onto the display. It uses its OWN gfx
    // connection so it doesn't share `gfx` with the main thread. While active,
    // the normal background redraws (ticker + keypress) are suppressed so they
    // don't fight the animation.
    let nyan_active = Arc::new(AtomicBool::new(false));
    {
        let nyan_active = nyan_active.clone();
        let xns_nyan = xous_names::XousNames::new().unwrap();
        std::thread::spawn(move || {
            let tt = ticktimer::Ticktimer::new().unwrap();
            let gfx = Gfx::new(&xns_nyan).unwrap();
            let mut frame = 0usize;
            loop {
                if nyan_active.load(Ordering::Relaxed) {
                    draw_frame(&gfx, &nyan::FRAMES[frame]);
                    frame = (frame + 1) % nyan::FRAME_COUNT;
                    tt.sleep_ms(100).ok(); // ~10 fps, matches the gif's 100ms/frame
                } else {
                    frame = 0;
                    tt.sleep_ms(150).ok();
                }
            }
        });
    }

    // ---- Button mapping -------------------------------------------------------
    // The physical button->char mapping is not documented, so we accept a set
    // of plausible codes per action.
    //   LEFT  = previous pattern:  '←', 'a', '1'
    //   RIGHT = next pattern:      '→', 'd', '3'
    //   UP    = brighter:          '↑'
    //   DOWN  = dimmer:            '↓'
    //   PROG  = camera:            '🔥'
    const PREV_KEYS: &[char] = &['←', 'a', '1'];
    const NEXT_KEYS: &[char] = &['→', 'd', '3'];
    const UP_KEY: char = '↑';
    const DOWN_KEY: char = '↓';
    const CAMERA_KEY: char = '🔥';

    // ---- Brightness -----------------------------------------------------------
    // 10 steps of 10%. Minimum floor is 10% (never fully off via dimming).
    // Values are 0..255; 10% ≈ 26, 100% = 255.
    const BRIGHTNESS_MIN: u8 = 26; // ~10%
    const BRIGHTNESS_MAX: u8 = 255; // 100%
    const BRIGHTNESS_STEP: u8 = 26; // ~10% per press
    let mut brightness: u8 = BRIGHTNESS_MAX; // starts at max

    // ---- Hold gestures (auto-repeat based) ------------------------------------
    // Held arrow keys auto-repeat, delivering a stream of the same char tens of
    // ms apart. We track a per-key streak: consecutive repeats within
    // HOLD_REPEAT_GAP_MS extend it; any gap or different key resets it. Elapsed
    // wall-time since the streak start is the real "held for N seconds" gate.
    const HOLD_REPEAT_GAP_MS: u64 = 400;
    // DOWN held while ALREADY at minimum brightness for this long => power off.
    const POWEROFF_HOLD_MS: u64 = 3000;
    // UP held while ALREADY at maximum brightness for this long => easter egg.
    const EASTER_HOLD_MS: u64 = 10000;

    // Streak state (shared for whichever key is currently being held).
    let mut hold_key: char = '\u{0000}';
    let mut hold_first_ms: u64 = 0;
    let mut hold_last_ms: u64 = 0;
    // Latches so the poweroff / easter actions fire once per sustained hold.
    let mut easter_active = false;

    let mut msg_opt = None;
    loop {
        xous::reply_and_receive_next(sid, &mut msg_opt).unwrap();
        let opcode = {
            let msg = msg_opt.as_mut().unwrap();
            num_traits::FromPrimitive::from_usize(msg.body.id()).unwrap_or(LedAppOp::Invalid)
        };
        match opcode {
            LedAppOp::KeyPress => {
                let msg = msg_opt.as_mut().unwrap();
                if let Some(scalar) = msg.body.scalar_message() {
                    let k = char::from_u32(scalar.arg1 as u32).unwrap_or('\u{0000}');
                    log::info!("key {:#x} -> {:?}", scalar.arg1, k);

                    let now = tt.elapsed_ms();
                    // Maintain the hold streak for the current key.
                    if k == hold_key && now.saturating_sub(hold_last_ms) <= HOLD_REPEAT_GAP_MS {
                        // continuing an existing hold of the same key
                    } else {
                        hold_key = k;
                        hold_first_ms = now;
                    }
                    hold_last_ms = now;
                    let held_ms = now.saturating_sub(hold_first_ms);

                    if k == UP_KEY {
                        if brightness >= BRIGHTNESS_MAX {
                            // Already at max. Sustained hold => easter egg.
                            brightness = BRIGHTNESS_MAX;
                            if !easter_active && held_ms >= EASTER_HOLD_MS {
                                easter_active = true;
                                log::info!("easter egg: rainbow + nyan (held UP at max {}ms)", held_ms);
                                // Force the Rainbow pattern.
                                xous::try_send_message(
                                    led_ctl,
                                    xous::Message::new_scalar(
                                        LedCtlOp::SetPattern as usize,
                                        PatternKind::Rainbow as u32 as usize,
                                        0,
                                        0,
                                        0,
                                    ),
                                )
                                .ok();
                                // Start the nyan animation (player thread owns gfx).
                                nyan_active.store(true, Ordering::Relaxed);
                            }
                        } else {
                            brightness = brightness.saturating_add(BRIGHTNESS_STEP).min(BRIGHTNESS_MAX);
                            log::info!("brightness up -> {}", brightness);
                            xous::try_send_message(
                                led_ctl,
                                xous::Message::new_scalar(
                                    LedCtlOp::SetBrightness as usize,
                                    brightness as usize,
                                    0,
                                    0,
                                    0,
                                ),
                            )
                            .ok();
                            if !nyan_active.load(Ordering::Relaxed) {
                                draw_background(&gfx);
                            }
                        }
                    } else if k == DOWN_KEY {
                        if brightness <= BRIGHTNESS_MIN {
                            // Already at min. Sustained hold => power off.
                            brightness = BRIGHTNESS_MIN;
                            if held_ms >= POWEROFF_HOLD_MS {
                                log::info!("power off (held DOWN at min {}ms)", held_ms);
                                nyan_active.store(false, Ordering::Relaxed);
                                gfx.clear().ok();
                                gfx.flush().ok();
                                // Wait for button release so the held key isn't
                                // seen as an immediate wake source by deep_sleep
                                // (which wakes on any button).
                                log::info!("power off: waiting 3s for button release before sleep");
                                tt.sleep_ms(3000).ok();
                                log::info!("power off: sending DeepSleep to susres server");
                                use num_traits::ToPrimitive;
                                let xns2 = xous_names::XousNames::new().unwrap();
                                if let Ok(conn) = xns2
                                    .request_connection_blocking(susres::api::SERVER_NAME_SUSRES)
                                {
                                    xous::send_message(
                                        conn,
                                        xous::Message::new_scalar(
                                            susres::api::Opcode::PlatformSpecific.to_usize().unwrap(),
                                            bao1x_hal::clocks::ClockOp::DeepSleep.to_usize().unwrap(),
                                            0,
                                            0,
                                            0,
                                        ),
                                    )
                                    .ok();
                                }
                                loop {
                                    tt.sleep_ms(1000).ok();
                                }
                            }
                        } else {
                            brightness = brightness.saturating_sub(BRIGHTNESS_STEP).max(BRIGHTNESS_MIN);
                            log::info!("brightness down -> {}", brightness);
                            xous::try_send_message(
                                led_ctl,
                                xous::Message::new_scalar(
                                    LedCtlOp::SetBrightness as usize,
                                    brightness as usize,
                                    0,
                                    0,
                                    0,
                                ),
                            )
                            .ok();
                            if !nyan_active.load(Ordering::Relaxed) {
                                draw_background(&gfx);
                            }
                        }
                    } else if k == CAMERA_KEY {
                        // Camera mode: acquire a QR code, then redraw the badge.
                        // Cancel the easter egg / nyan animation.
                        nyan_active.store(false, Ordering::Relaxed);
                        easter_active = false;
                        log::info!("entering camera mode");
                        match gfx.acquire_qr() {
                            Ok(qr) => log::info!("camera returned: {:?}", qr.content),
                            Err(e) => log::warn!("camera acquire_qr failed: {:?}", e),
                        }
                        draw_background(&gfx);
                    } else if PREV_KEYS.contains(&k) {
                        // Left = previous pattern. Cancel the easter egg so the
                        // static background returns.
                        if nyan_active.swap(false, Ordering::Relaxed) {
                            easter_active = false;
                            draw_background(&gfx);
                        }
                        xous::try_send_message(
                            led_ctl,
                            xous::Message::new_scalar(LedCtlOp::Prev as usize, 0, 0, 0, 0),
                        )
                        .ok();
                        if !nyan_active.load(Ordering::Relaxed) {
                            draw_background(&gfx);
                        }
                    } else if NEXT_KEYS.contains(&k) {
                        // Right = next pattern. Cancel the easter egg.
                        if nyan_active.swap(false, Ordering::Relaxed) {
                            easter_active = false;
                            draw_background(&gfx);
                        }
                        xous::try_send_message(
                            led_ctl,
                            xous::Message::new_scalar(LedCtlOp::Next as usize, 0, 0, 0, 0),
                        )
                        .ok();
                        if !nyan_active.load(Ordering::Relaxed) {
                            draw_background(&gfx);
                        }
                    } else {
                        // Unknown key; keep the display persistent unless nyan
                        // is animating.
                        if !nyan_active.load(Ordering::Relaxed) {
                            draw_background(&gfx);
                        }
                    }
                }
            }
            LedAppOp::Redraw => {
                // The periodic ticker only refreshes the static background when
                // the nyan animation is not running (the player owns the screen
                // while active).
                if !nyan_active.load(Ordering::Relaxed) {
                    draw_background(&gfx);
                }
            }
            LedAppOp::Invalid => {
                log::error!("Invalid LED app operation");
            }
        }
    }
}
