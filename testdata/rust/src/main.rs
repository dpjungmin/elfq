//! Sample program for exercising elfq.
//!
//! The Rust twin of `testdata/c/app.c`: it decodes a few hard-coded "sensor
//! frames", runs the samples through a filter pipeline and prints a report.
//! On top of the C sample's call-graph shapes it adds Rust-specific ones:
//! generic functions instantiated several times, calls through trait objects,
//! and panic paths that pull in `core::fmt`. `testdata/README.md` lists which
//! function demonstrates what.
//!
//! The same file builds for x86-64 Linux and for a bare-metal Cortex-M4
//! (`thumbv7em-none-eabihf`).
#![cfg_attr(target_os = "none", no_std, no_main)]
// Index loops are deliberate: they create the bounds checks and panic paths
// the sample is meant to contain. Empty loops are how bare-metal code halts.
#![allow(clippy::needless_range_loop, clippy::empty_loop)]

use core::hint::black_box;
use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

// ---------------------------------------------------------------------------
// Shared helper: called from many places, so it is never exclusive to one.
// ---------------------------------------------------------------------------

#[inline(never)]
fn log_msg(msg: &str) {
    platform::write(msg.as_bytes());
}

// ---------------------------------------------------------------------------
// Event handlers: only reachable through a function pointer table.
// ---------------------------------------------------------------------------

const EVT_TIMER: usize = 0;
const EVT_BUTTON: usize = 1;

static TIMER_TICKS: AtomicU32 = AtomicU32::new(0);
static BUTTON_PRESSES: AtomicU32 = AtomicU32::new(0);
static DATA_EVENTS: AtomicU32 = AtomicU32::new(0);

#[inline(never)]
fn on_timer(arg: u32) {
    TIMER_TICKS.fetch_add(arg, Relaxed);
}

#[inline(never)]
fn on_button(arg: u32) {
    BUTTON_PRESSES.fetch_add(arg, Relaxed);
    log_msg("button\n");
}

#[inline(never)]
fn on_data(arg: u32) {
    DATA_EVENTS.fetch_add(arg, Relaxed);
}

static HANDLERS: [fn(u32); 3] = [on_timer, on_button, on_data];

/// The call through `HANDLERS` is indirect: its target is unknown statically.
#[inline(never)]
fn dispatch_event(id: usize, arg: u32) {
    if let Some(handler) = HANDLERS.get(id) {
        handler(arg);
    }
}

/// Ends in a direct tail call, which the compiler emits as a jump.
#[inline(never)]
pub fn app_tick(now: u32) {
    if now.is_multiple_of(100) {
        log_msg("tick\n");
    }
    dispatch_event(EVT_TIMER, now);
}

// ---------------------------------------------------------------------------
// process_frame: the "expensive function" a user would investigate.
// ---------------------------------------------------------------------------

const FRAME_MAGIC: u16 = 0xE1F0;
const HEADER_SIZE: usize = 5;
const CRC_SIZE: usize = 4;
const MAX_SAMPLES: usize = 32;

/// 1 KiB of .rodata that only `crc32` reads, computed at compile time.
static CRC_TABLE: [u32; 256] = make_crc_table();

const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

#[inline(never)]
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = CRC_TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

struct Header {
    length: usize,
    kind: usize,
}

#[inline(never)]
fn decode_header(buf: &[u8]) -> Option<Header> {
    if buf.len() < HEADER_SIZE {
        return None;
    }
    let magic = u16::from_le_bytes([buf[0], buf[1]]);
    let length = u16::from_le_bytes([buf[2], buf[3]]) as usize;
    (magic == FRAME_MAGIC).then_some(Header {
        length,
        kind: buf[4] as usize,
    })
}

static FIR_COEFFS: [i32; 16] = [
    -120, -340, 210, 1830, 4100, 6620, 8300, 8900, 8300, 6620, 4100, 1830, 210, -340, -120, 40,
];

/// Only called by `apply_filter`, so it is exclusive to `process_frame` too.
#[inline(never)]
fn filter_tap(x: &[i16], i: usize) -> i32 {
    let mut acc = 0;
    for k in 0..=i.min(15) {
        acc += x[i - k] as i32 * FIR_COEFFS[k];
    }
    acc
}

#[inline(never)]
fn apply_filter(samples: &mut [i16]) {
    let mut out = [0i16; MAX_SAMPLES];
    for i in 0..samples.len() {
        out[i] = saturate(filter_tap(samples, i) >> 15);
    }
    samples.copy_from_slice(&out[..samples.len()]);
}

#[inline(always)]
fn saturate(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Inlined into `process_frame`: its code counts toward that function's size.
#[inline(always)]
fn unpack_samples(payload: &[u8], out: &mut [i16; MAX_SAMPLES]) -> usize {
    let n = (payload.len() / 2).min(MAX_SAMPLES);
    for (slot, pair) in out.iter_mut().zip(payload.chunks_exact(2)) {
        *slot = i16::from_le_bytes([pair[0], pair[1]]);
    }
    n
}

/// A processing stage called through a trait object (a vtable).
trait Stage {
    fn run(&mut self, samples: &mut [i16]);
}

struct Gain(i16);

impl Stage for Gain {
    fn run(&mut self, samples: &mut [i16]) {
        for s in samples.iter_mut() {
            *s = s.saturating_mul(self.0);
        }
    }
}

struct DcBlock {
    last: i32,
}

impl Stage for DcBlock {
    fn run(&mut self, samples: &mut [i16]) {
        for s in samples.iter_mut() {
            let v = *s as i32;
            *s = saturate(v - self.last);
            self.last = v;
        }
    }
}

static SAMPLE_COUNT: AtomicU32 = AtomicU32::new(0);
static PEAK: AtomicU32 = AtomicU32::new(0);

/// Inlined into `process_frame` as well.
#[inline(always)]
fn update_stats(samples: &[i16]) {
    for &s in samples {
        PEAK.fetch_max(s.unsigned_abs() as u32, Relaxed);
    }
    SAMPLE_COUNT.fetch_add(samples.len() as u32, Relaxed);
}

static FRAMES_OK: AtomicU32 = AtomicU32::new(0);
static FRAMES_BAD: AtomicU32 = AtomicU32::new(0);

#[derive(Debug)]
enum FrameError {
    BadHeader,
    BadCrc,
}

#[inline(never)]
fn process_frame(buf: &[u8], stages: &mut [&mut dyn Stage]) -> Result<usize, FrameError> {
    let Some(header) = decode_header(buf) else {
        FRAMES_BAD.fetch_add(1, Relaxed);
        log_msg("bad header\n");
        return Err(FrameError::BadHeader);
    };
    // Slicing past the end panics: this is a deliberate panic path.
    let payload = &buf[HEADER_SIZE..HEADER_SIZE + header.length];
    let crc = &buf[HEADER_SIZE + header.length..][..CRC_SIZE];
    if crc32(payload) != u32::from_le_bytes([crc[0], crc[1], crc[2], crc[3]]) {
        FRAMES_BAD.fetch_add(1, Relaxed);
        log_msg("bad crc\n");
        return Err(FrameError::BadCrc);
    }

    let mut samples = [0i16; MAX_SAMPLES];
    let n = unpack_samples(payload, &mut samples);
    let samples = &mut samples[..n];
    apply_filter(samples);
    for stage in stages.iter_mut() {
        stage.run(samples);
    }
    update_stats(samples);
    dispatch_event(header.kind % HANDLERS.len(), n as u32);
    FRAMES_OK.fetch_add(1, Relaxed);
    Ok(n)
}

// ---------------------------------------------------------------------------
// Generic code: one source function, several machine-code copies.
// ---------------------------------------------------------------------------

/// Instantiated for u8, u16 and u32, so the binary has three copies.
#[inline(never)]
fn checksum<T: Copy + Into<u32>>(data: &[T]) -> u32 {
    data.iter()
        .fold(0u32, |acc, &x| acc.rotate_left(5) ^ x.into())
}

// ---------------------------------------------------------------------------
// Reporting.
// ---------------------------------------------------------------------------

struct Node {
    left: Option<&'static Node>,
    right: Option<&'static Node>,
}

static LEAF_A: Node = Node {
    left: None,
    right: None,
};
static LEAF_B: Node = Node {
    left: None,
    right: None,
};
static LEAF_C: Node = Node {
    left: None,
    right: None,
};
static INNER: Node = Node {
    left: Some(&LEAF_B),
    right: Some(&LEAF_C),
};
static ROOT: Node = Node {
    left: Some(&LEAF_A),
    right: Some(&INNER),
};

/// Directly recursive: a self-loop in the call graph.
#[inline(never)]
fn count_nodes(node: Option<&Node>) -> u32 {
    match node {
        None => 0,
        Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right),
    }
}

/// Only called by `report`.
#[inline(never)]
fn format_u32(out: &mut [u8], mut v: u32) -> usize {
    let mut digits = [0u8; 10];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in 0..n {
        out[i] = digits[n - 1 - i];
    }
    n
}

#[inline(never)]
fn report() {
    let mut line = [0u8; 128];
    let mut len = 0;
    let fields: [(&str, u32); 5] = [
        ("ok=", FRAMES_OK.load(Relaxed)),
        (" bad=", FRAMES_BAD.load(Relaxed)),
        (" samples=", SAMPLE_COUNT.load(Relaxed)),
        (" peak=", PEAK.load(Relaxed)),
        (" nodes=", count_nodes(Some(&ROOT))),
    ];
    for (label, value) in fields {
        line[len..len + label.len()].copy_from_slice(label.as_bytes());
        len += label.len();
        len += format_u32(&mut line[len..], value);
    }
    line[len] = b'\n';
    len += 1;
    platform::write(&line[..len]);
}

// ---------------------------------------------------------------------------
// Entry point.
// ---------------------------------------------------------------------------

static FRAME_A: [u8; 25] = [
    0xf0, 0xe1, 0x10, 0x00, 0x01, 0x64, 0x00, 0x38, 0xff, 0x2c, 0x01, 0x70, 0xfe, 0xf4, 0x01, 0xa8,
    0xfd, 0xbc, 0x02, 0xe0, 0xfc, 0xa4, 0xf6, 0x8a, 0xf6,
];
static FRAME_B: [u8; 17] = [
    0xf0, 0xe1, 0x08, 0x00, 0x02, 0xe8, 0x03, 0xd0, 0x07, 0x48, 0xf4, 0xa0, 0x0f, 0xb9, 0x31, 0x34,
    0x76,
];
/// Bad magic: exercises the error path.
static FRAME_C: [u8; 5] = [0; 5];

#[inline(never)]
fn app_main() {
    let mut gain = Gain(2);
    let mut dc = DcBlock { last: 0 };
    let mut stages: [&mut dyn Stage; 2] = [&mut gain, &mut dc];

    for frame in [&FRAME_A[..], &FRAME_B[..], &FRAME_C[..]] {
        let _ = process_frame(frame, &mut stages);
    }
    for t in (0..=200).step_by(50) {
        app_tick(t);
    }
    dispatch_event(EVT_BUTTON, 1);
    // black_box keeps the optimizer from computing the checksums at compile time.
    let sum = checksum(black_box(&FRAME_A[..]))
        ^ checksum(black_box(&[1u16, 2, 3][..]))
        ^ checksum(black_box(&[4u32, 5][..]));
    DATA_EVENTS.fetch_add(sum & 1, Relaxed);
    report();
}

#[cfg(not(target_os = "none"))]
fn main() {
    app_main();
}

// ---------------------------------------------------------------------------
// Platform layer.
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "none"))]
mod platform {
    use std::io::Write;

    pub fn write(bytes: &[u8]) {
        let _ = std::io::stdout().write_all(bytes);
    }
}

/// Bare-metal Cortex-M4 (STM32F4-style memory map). Interrupt handlers are
/// reached only through the vector table, which is data, not code.
#[cfg(target_os = "none")]
mod platform {
    use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
    use core::sync::atomic::{AtomicU32, Ordering::Relaxed};

    const USART2_SR: *const u32 = 0x4000_4400 as *const u32;
    const USART2_DR: *mut u32 = 0x4000_4404 as *mut u32;
    const USART_SR_TXE: u32 = 1 << 7;

    pub fn write(bytes: &[u8]) {
        for &b in bytes {
            // SAFETY: fixed MMIO addresses of the UART on this chip.
            unsafe {
                while read_volatile(USART2_SR) & USART_SR_TXE == 0 {}
                write_volatile(USART2_DR, b as u32);
            }
        }
    }

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        crate::log_msg("panic\n");
        loop {}
    }

    // Provided by link.x.
    unsafe extern "C" {
        static mut _sdata: u32;
        static mut _edata: u32;
        static _sidata: u32;
        static mut _sbss: u32;
        static mut _ebss: u32;
    }

    /// # Safety
    /// Only the hardware may call this, once, out of reset.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Reset() -> ! {
        // SAFETY: runs before anything reads .data or .bss.
        unsafe {
            let mut src = addr_of!(_sidata);
            let mut dst = addr_of_mut!(_sdata);
            while dst < addr_of_mut!(_edata) {
                dst.write_volatile(src.read());
                dst = dst.add(1);
                src = src.add(1);
            }
            let mut dst = addr_of_mut!(_sbss);
            while dst < addr_of_mut!(_ebss) {
                dst.write_volatile(0);
                dst = dst.add(1);
            }
        }
        crate::app_main();
        loop {}
    }

    #[unsafe(link_section = ".vector_table.reset_vector")]
    #[unsafe(no_mangle)]
    pub static RESET_VECTOR: unsafe extern "C" fn() -> ! = Reset;

    static SYSTICK_COUNT: AtomicU32 = AtomicU32::new(0);

    extern "C" fn default_handler() {
        loop {}
    }

    extern "C" fn sys_tick() {
        crate::app_tick(SYSTICK_COUNT.fetch_add(1, Relaxed) + 1);
    }

    /// Exceptions 2 (NMI) through 15 (SysTick).
    #[unsafe(link_section = ".vector_table.exceptions")]
    #[unsafe(no_mangle)]
    pub static EXCEPTIONS: [Option<extern "C" fn()>; 14] = [
        Some(default_handler), // NMI
        Some(default_handler), // HardFault
        Some(default_handler), // MemManage
        Some(default_handler), // BusFault
        Some(default_handler), // UsageFault
        None,
        None,
        None,
        None,
        Some(default_handler), // SVCall
        Some(default_handler), // DebugMonitor
        None,
        Some(default_handler), // PendSV
        Some(sys_tick),        // SysTick
    ];
}
