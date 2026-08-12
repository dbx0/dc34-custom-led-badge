//! Slim WS2812 LED driver over the BIO co-processor.
//!
//! Adapted from dc34-console's `Lightgenes`, but with all gene/meiosis/
//! express/PDDB code removed. It knows how to:
//!   * claim a BIO core + FIFO1 + one dynamic pin,
//!   * load one of the self-contained animation programs,
//!   * push the FIFO1 configuration handshake the programs wait on
//!     (pin number, then LED count),
//!   * hot-swap the running program at runtime (`set_pattern`).

mod breathing;
mod brrunner;
mod cylon;
mod police;
mod rainbow;

use arbitrary_int::{Number, u5};
use bao1x_api::bio::*;
use bao1x_api::bio_resources::*;
use bao1x_hal::bio::{Bio, CoreCsr};

/// The LED animation programs that can run on the BIO co-processor.
///
/// Each variant is a self-contained animation that draws a fixed pattern.
/// They all take the same FIFO1 configuration handshake (pin number, then
/// LED count) and continuously drive the strip on their own once started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PatternKind {
    Police = 0,
    Rainbow = 1,
    Cylon = 2,
    Breathing = 3,
    BrRunner = 4,
}

/// The ordered list the next()/prev() controls cycle through.
pub const PATTERN_ORDER: &[PatternKind] = &[
    PatternKind::Police,
    PatternKind::Rainbow,
    PatternKind::Cylon,
    PatternKind::Breathing,
    PatternKind::BrRunner,
];

/// The next pattern after `p` in `PATTERN_ORDER`, wrapping around.
pub fn next(p: PatternKind) -> PatternKind {
    let idx = PATTERN_ORDER.iter().position(|&k| k == p).unwrap_or(0);
    PATTERN_ORDER[(idx + 1) % PATTERN_ORDER.len()]
}

/// The previous pattern before `p` in `PATTERN_ORDER`, wrapping around.
pub fn prev(p: PatternKind) -> PatternKind {
    let idx = PATTERN_ORDER.iter().position(|&k| k == p).unwrap_or(0);
    PATTERN_ORDER[(idx + PATTERN_ORDER.len() - 1) % PATTERN_ORDER.len()]
}

impl PatternKind {
    #[allow(dead_code)]
    pub fn from_u32(v: u32) -> Option<Self> {        match v {
            0 => Some(PatternKind::Police),
            1 => Some(PatternKind::Rainbow),
            2 => Some(PatternKind::Cylon),
            3 => Some(PatternKind::Breathing),
            4 => Some(PatternKind::BrRunner),
            _ => None,
        }
    }

    /// The BIO machine code for this pattern.
    fn bio_code(&self) -> (&'static [u8], Option<u32>) {
        match self {
            PatternKind::Police => police::police_bio_code(),
            PatternKind::Rainbow => rainbow::rainbow_bio_code(),
            PatternKind::Cylon => cylon::cylon_bio_code(),
            PatternKind::Breathing => breathing::breathing_bio_code(),
            PatternKind::BrRunner => brrunner::brrunner_bio_code(),
        }
    }
}

pub struct LedDriver {
    bio_ss: Bio,
    bio_pin: u5,
    // handles have to be kept around or else the underlying CSR is dropped
    _tx_handle: CoreHandle,
    // the CoreCsr is a convenience object that manages the CSR view of the handle
    tx: CoreCsr,
    _rx_handle: CoreHandle,
    #[allow(dead_code)]
    rx: CoreCsr,
    // tracks the resources used by the object
    resource_grant: ResourceGrant,
    // the LED count, kept so a pattern switch can re-send the FIFO1 handshake
    led_count: u8,
    config: CoreConfig,
    // which animation is currently loaded on the BIO core
    pattern: PatternKind,
    // current LED brightness 0..255, kept so a pattern switch re-sends it
    brightness: u8,
}

impl Resources for LedDriver {
    fn resource_spec() -> ResourceSpec {
        ResourceSpec {
            claimer: "LedDriver".to_string(),
            cores: vec![CoreRequirement::Any],
            fifos: vec![Fifo::Fifo1],
            static_pins: vec![],
            dynamic_pin_count: 1,
        }
    }
}

impl Drop for LedDriver {
    fn drop(&mut self) {
        for &core in self.resource_grant.cores.iter() {
            self.bio_ss.de_init_core(core).unwrap();
        }
        self.bio_ss.release_dynamic_pin(self.bio_pin.as_u8(), &LedDriver::resource_spec().claimer).unwrap();
        self.bio_ss.release_resources(self.resource_grant.grant_id).unwrap();
    }
}

impl LedDriver {
    pub fn new(
        bio_pin: u5,
        led_count: u8,
        io_mode: Option<IoConfigMode>,
        initial_pattern: PatternKind,
        initial_brightness: u8,
    ) -> Result<Self, BioError> {
        let mut bio_ss = Bio::new();
        // claim core resource and initialize it
        let resource_grant = bio_ss.claim_resources(&Self::resource_spec())?;
        log::info!("using core: {:?}", resource_grant.cores[0]);
        // 150 ns nominal quantum
        let config = CoreConfig { clock_mode: bao1x_api::bio::ClockMode::TargetFreqInt(6_666_667) };
        // Load whichever pattern was requested at startup. Every pattern takes
        // the same FIFO1 configuration handshake (pin number, then LED count,
        // pushed just below), so the rest of the driver works unmodified
        // regardless of which one is loaded. Patterns can be swapped at runtime
        // with set_pattern().
        let actual = bio_ss.init_core(resource_grant.cores[0], initial_pattern.bio_code(), config)?;
        log::info!("BIO init_core actual quantum freq: {:?} Hz (requested 6666667)", actual);
        bio_ss.set_core_run_state(&resource_grant, true);

        // claim pin resource - this only claims the resource, it does not configure it
        bio_ss.claim_dynamic_pin(bio_pin.as_u8(), &LedDriver::resource_spec().claimer)?;
        // now configure the claimed resource
        let mut io_config = IoConfig::default();
        io_config.mapped = 1 << bio_pin.as_u32();
        io_config.mode = io_mode.unwrap_or(IoConfigMode::Overwrite);
        bio_ss.setup_io_config(io_config).unwrap();

        // safety: fifo handles are stored in this object so they aren't Drop'd
        // before the object is destroyed
        let tx_handle = unsafe { bio_ss.get_core_handle(Fifo::Fifo1) }?.expect("Didn't get FIFO1 handle");
        let rx_handle = unsafe { bio_ss.get_core_handle(Fifo::Fifo2) }?.expect("Didn't get FIFO2 handle");

        let mut tx = CoreCsr::from_handle(&tx_handle);
        // set FIFO1 event trigger level, so the event triggers if there is more
        // than 0 items in the FIFO. The BIO core uses this to know if there are
        // values waiting for it in FIFO1.
        bio_ss
            .setup_fifo_event_triggers(FifoEventConfig {
                which: Fifo::Fifo1,
                trigger_slot: TriggerSlot::new_with_raw_value(0),
                level: FifoLevel::new_with_raw_value(1),
                trigger_less_than: false,
                trigger_greater_than: true,
                trigger_equal_to: true,
            })
            .expect("couldn't set FIFO trigger configuration");

        tx.csr.wo(utralib::utra::bio_bdma::SFR_TXF1, bio_pin.as_u32());
        tx.csr.wo(utralib::utra::bio_bdma::SFR_TXF1, led_count as u32);
        // third handshake word: initial brightness (0..255)
        tx.csr.wo(utralib::utra::bio_bdma::SFR_TXF1, initial_brightness as u32);

        Ok(Self {
            bio_ss,
            bio_pin,
            tx: CoreCsr::from_handle(&tx_handle),
            rx: CoreCsr::from_handle(&rx_handle),
            // safety: tx and rx are wrapped in CSR objects whose lifetime matches that of the handles
            _tx_handle: tx_handle,
            _rx_handle: rx_handle,
            resource_grant,
            led_count,
            config,
            pattern: initial_pattern,
            brightness: initial_brightness,
        })
    }

    /// The pattern currently loaded on the BIO core.
    #[allow(dead_code)]
    pub fn pattern(&self) -> PatternKind { self.pattern }

    /// Swap the running BIO program to a different pattern at runtime.
    ///
    /// The BIO core runs exactly one program, chosen at init_core() time, so
    /// switching means tearing the core down and bringing it back up with new
    /// code. After re-init we must re-send the FIFO1 configuration handshake
    /// (pin, then LED count) because the freshly-loaded program blocks waiting
    /// for it, exactly as it did at first start.
    pub fn set_pattern(&mut self, pattern: PatternKind) -> Result<(), BioError> {
        if pattern == self.pattern {
            return Ok(());
        }
        log::info!("switching LED pattern {:?} -> {:?}", self.pattern, pattern);

        // Stop and tear down the currently-loaded core program.
        self.bio_ss.set_core_run_state(&self.resource_grant, false);
        self.bio_ss.de_init_core(self.resource_grant.cores[0])?;

        // Bring the core back up with the new program, same clock config.
        let actual = self.bio_ss.init_core(self.resource_grant.cores[0], pattern.bio_code(), self.config)?;
        log::info!("BIO set_pattern init_core actual quantum freq: {:?} Hz (requested 6666667)", actual);

        // Re-assert the IO pin mapping. de_init_core can leave the IO matrix in
        // a state where the pin is no longer driven by the (freshly re-loaded)
        // core, which would make the new program run but light nothing. This
        // mirrors the setup done in new().
        let mut io_config = IoConfig::default();
        io_config.mapped = 1 << self.bio_pin.as_u32();
        io_config.mode = IoConfigMode::Overwrite;
        self.bio_ss.setup_io_config(io_config).ok();

        self.bio_ss.set_core_run_state(&self.resource_grant, true);

        // Re-send the configuration handshake the new program is waiting on.
        self.tx.csr.wo(utralib::utra::bio_bdma::SFR_TXF1, self.bio_pin.as_u32());
        self.tx.csr.wo(utralib::utra::bio_bdma::SFR_TXF1, self.led_count as u32);
        // third handshake word: current brightness, so the new pattern starts
        // at the same level the user last selected.
        self.tx.csr.wo(utralib::utra::bio_bdma::SFR_TXF1, self.brightness as u32);

        self.pattern = pattern;
        Ok(())
    }

    /// Current brightness, 0..255.
    #[allow(dead_code)]
    pub fn brightness(&self) -> u8 { self.brightness }

    /// Update the running pattern's brightness (0..255) at runtime.
    ///
    /// Sends a brightness-update word to the BIO program over FIFO1, tagged
    /// with bit 30 so the pattern's drain loop recognizes it (bit 31 is the
    /// pause/control tag; plain pin/count values have neither high bit set).
    pub fn set_brightness(&mut self, level: u8) {
        self.brightness = level;
        let word: u32 = 0x4000_0000 | (level as u32 & 0xFF);
        // Don't overflow the FIFO; wait for room if needed.
        while self.tx.csr.rf(utralib::utra::bio_bdma::SFR_FLEVEL_PCLK_REGFIFO_LEVEL1) > 7 {}
        self.tx.csr.wo(utralib::utra::bio_bdma::SFR_TXF1, word);
    }

    /// Move to the next pattern in `PATTERN_ORDER`, wrapping around.
    pub fn next_pattern(&mut self) -> Result<(), BioError> {
        let p = next(self.pattern);
        self.set_pattern(p)
    }

    /// Move to the previous pattern in `PATTERN_ORDER`, wrapping around.
    pub fn prev_pattern(&mut self) -> Result<(), BioError> {
        let p = prev(self.pattern);
        self.set_pattern(p)
    }
}
