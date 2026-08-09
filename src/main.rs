mod bio;
mod background;
mod leds;

use num_derive::FromPrimitive;
use ux_api::service::gfx::Gfx;

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
    // Defensively clear any lingering dry_run state left by another gfx client;
    // while dry_run is set, Flush is a no-op and nothing ever reaches the panel.
    if let Err(e) = gfx.dry_run(false) {
        log::warn!("gfx.dry_run(false) err: {:?}", e);
    }
    if let Err(e) = gfx.clear() {
        log::warn!("gfx.clear err: {:?}", e);
    }
    if let Err(e) = gfx.bitmap(&background::BITMAP, None, None) {
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

    // Candidate characters that should map to prev / next / camera. The real
    // button->char mapping on this hardware is not documented, so we accept a
    // set of plausible codes so at least one physical button works for each
    // action.
    //   button1 (prev):  '←', '↑', 'a', '1'
    //   button3 (next):  '→', 'd', '3'
    //   button2/PROG (camera): '🔥'
    //
    // NOTE: the DOWN arrow '↓' is intentionally NOT in NEXT_KEYS: it is
    // dedicated to the press-and-hold power-off gesture (see below). A single
    // '↓' tap does nothing; you must HOLD '↓' for ~1.5-2s to power off.
    const PREV_KEYS: &[char] = &['←', '↑', 'a', '1'];
    const NEXT_KEYS: &[char] = &['→', 'd', '3'];
    const CAMERA_KEY: char = '🔥';

    // ---- Press-and-hold power-off configuration -------------------------------
    // The DOWN arrow auto-repeats when physically held: holding it delivers a
    // stream of '↓' KeyPress events tens of ms apart. We detect a genuine hold
    // (as opposed to a fast double-tap) by requiring BOTH:
    //   1. a run of at least POWER_OFF_HOLD_COUNT consecutive '↓' repeats where
    //      each successive repeat arrives within POWER_OFF_REPEAT_GAP_MS of the
    //      previous one (any longer gap, or any other key, resets the run), AND
    //   2. at least POWER_OFF_HOLD_MS of wall-clock time elapsed since the first
    //      '↓' in that run.
    // The wall-time gate is the real "must hold ~1.5s" guarantee; the repeat
    // count guards against a single stray event tripping the timer math.
    const POWER_OFF_KEY: char = '↓';
    const POWER_OFF_HOLD_COUNT: u32 = 10;
    const POWER_OFF_REPEAT_GAP_MS: u64 = 400;
    const POWER_OFF_HOLD_MS: u64 = 1500;

    // Hold-tracking state for the power-off gesture.
    let mut down_hold_count: u32 = 0;
    let mut down_hold_first_ms: u64 = 0;
    let mut down_hold_last_ms: u64 = 0;

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

                    // ---- Press-and-hold power-off detection ------------------
                    // Hold DOWN ('↓') ~1.5-2s to power off. The key auto-repeats
                    // while held, so we count consecutive fast repeats and gate
                    // on elapsed wall-time. Any other key breaks the streak.
                    if k == POWER_OFF_KEY {
                        let now = tt.elapsed_ms();
                        if down_hold_count > 0
                            && now.saturating_sub(down_hold_last_ms) <= POWER_OFF_REPEAT_GAP_MS
                        {
                            // Continuing an existing hold streak.
                            down_hold_count += 1;
                        } else {
                            // Start (or restart) a hold streak.
                            down_hold_count = 1;
                            down_hold_first_ms = now;
                        }
                        down_hold_last_ms = now;

                        let held_ms = now.saturating_sub(down_hold_first_ms);
                        if down_hold_count >= POWER_OFF_HOLD_COUNT && held_ms >= POWER_OFF_HOLD_MS {
                            log::info!(
                                "power off requested (hold), shutting down (count={}, held={}ms)",
                                down_hold_count,
                                held_ms
                            );
                            // Blank the screen as a "powering off" indicator.
                            gfx.clear().ok();
                            gfx.flush().ok();
                            // Wait for the user to release the button before
                            // arming deep sleep. deep_sleep() wakes on ANY
                            // button press; if we sleep while DOWN is still
                            // held (auto-repeat), the held key is seen as an
                            // immediate wake event and the device restarts.
                            // Stock firmware delayed ~5s; 3s is a comfortable
                            // UX compromise that outlasts a normal hold+release.
                            log::info!(
                                "power off: waiting 3s for button release before sleep"
                            );
                            tt.sleep_ms(3000).ok();
                            log::info!("power off: sending DeepSleep to susres server");
                            // The susres server (in bao1x-hal-service) owns the
                            // ClockManagerImpl and its deep_sleep(). We cannot
                            // construct ClockManagerImpl here (its CSR pages are
                            // already mapped by that process -> MemoryInUse).
                            // Instead ask susres to power down, exactly as the
                            // stock power manager did.
                            use num_traits::ToPrimitive;
                            let xns2 = xous_names::XousNames::new().unwrap();
                            if let Ok(conn) = xns2
                                .request_connection_blocking(susres::api::SERVER_NAME_SUSRES)
                            {
                                xous::send_message(
                                    conn,
                                    xous::Message::new_scalar(
                                        susres::api::Opcode::PlatformSpecific
                                            .to_usize()
                                            .unwrap(),
                                        bao1x_hal::clocks::ClockOp::DeepSleep
                                            .to_usize()
                                            .unwrap(),
                                        0,
                                        0,
                                        0,
                                    ),
                                )
                                .ok();
                            }
                            // Device powers down; nothing past here runs. Loop
                            // just in case the message returned for some reason,
                            // so we don't fall through to more key handling with
                            // a half-shutdown state.
                            loop {
                                tt.sleep_ms(1000).ok();
                            }
                        }
                        // While detecting a hold, keep redrawing the background so
                        // the badge persists between repeats.
                        draw_background(&gfx);
                    } else if k == CAMERA_KEY {
                        // Any non-'↓' key breaks the power-off hold streak.
                        down_hold_count = 0;
                        // Camera mode: acquire a QR code, then redraw the badge.
                        log::info!("entering camera mode");
                        match gfx.acquire_qr() {
                            Ok(qr) => log::info!("camera returned: {:?}", qr.content),
                            Err(e) => log::warn!("camera acquire_qr failed: {:?}", e),
                        }
                        draw_background(&gfx);
                    } else if PREV_KEYS.contains(&k) {
                        down_hold_count = 0;
                        xous::try_send_message(
                            led_ctl,
                            xous::Message::new_scalar(LedCtlOp::Prev as usize, 0, 0, 0, 0),
                        )
                        .ok();
                        draw_background(&gfx);
                    } else if NEXT_KEYS.contains(&k) {
                        down_hold_count = 0;
                        xous::try_send_message(
                            led_ctl,
                            xous::Message::new_scalar(LedCtlOp::Next as usize, 0, 0, 0, 0),
                        )
                        .ok();
                        draw_background(&gfx);
                    } else {
                        down_hold_count = 0;
                        // Unknown key; still redraw so the background persists.
                        draw_background(&gfx);
                    }
                }
            }
            LedAppOp::Redraw => {
                draw_background(&gfx);
            }
            LedAppOp::Invalid => {
                log::error!("Invalid LED app operation");
            }
        }
    }
}
