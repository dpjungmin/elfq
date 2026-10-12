// Sample program for exercising elfq with C++ names.
//
// It decodes a few hard-coded "sensor frames", like the C and Rust samples,
// but each part exists to produce one kind of C++ symbol: namespaces, member
// functions, virtual functions and their vtables, templates, overloads,
// operators, lambdas, an anonymous namespace and a global object with a
// constructor. The table in testdata/README.md lists which is which.
//
// Built for x86-64 Linux only: the names are the same on every target.

#include <stddef.h>
#include <stdint.h>
#include <unistd.h>

#define NOINLINE __attribute__((noinline))
#define ALWAYS_INLINE inline __attribute__((always_inline))

namespace app {

// ---------------------------------------------------------------------------
// Overloads: one name, two functions. SYMBOL needs the parameters to pick one.
// ---------------------------------------------------------------------------

NOINLINE void log(const char *msg)
{
    size_t len = 0;

    while (msg[len] != '\0')
        len++;
    if (write(1, msg, len) < 0)
        return;
}

NOINLINE void log(uint32_t value)
{
    char digits[10];
    int i = 0;

    do {
        digits[i++] = static_cast<char>('0' + value % 10);
        value /= 10;
    } while (value != 0);
    char text[11];
    int n = 0;
    while (i > 0)
        text[n++] = digits[--i];
    if (write(1, text, static_cast<size_t>(n)) < 0)
        return;
}

// ---------------------------------------------------------------------------
// Virtual functions: process_frame calls run() through a Stage reference, so
// Gain::run and DcBlock::run are reached only through their vtables.
// ---------------------------------------------------------------------------

class Stage {
public:
    virtual int run(int16_t *samples, size_t n) = 0;
};

class Gain : public Stage {
public:
    explicit Gain(int16_t factor) : factor_(factor) {}
    NOINLINE int run(int16_t *samples, size_t n) override;

private:
    int16_t factor_;
};

class DcBlock : public Stage {
public:
    NOINLINE int run(int16_t *samples, size_t n) override;

private:
    int32_t last_ = 0;
};

int Gain::run(int16_t *samples, size_t n)
{
    for (size_t i = 0; i < n; i++)
        samples[i] = static_cast<int16_t>(samples[i] * factor_ / 16);
    return static_cast<int>(n);
}

int DcBlock::run(int16_t *samples, size_t n)
{
    for (size_t i = 0; i < n; i++) {
        int32_t v = samples[i];
        samples[i] = static_cast<int16_t>(v - last_);
        last_ = v;
    }
    return static_cast<int>(n);
}

// ---------------------------------------------------------------------------
// An anonymous namespace: crc32 and its table are local to this file.
// ---------------------------------------------------------------------------

namespace {

const uint32_t crc_table[256] = {
#include "../c/crc32_table.inc"
};

NOINLINE uint32_t crc32(const uint8_t *data, size_t len)
{
    uint32_t crc = 0xffffffffu;

    for (size_t i = 0; i < len; i++)
        crc = crc_table[(crc ^ data[i]) & 0xffu] ^ (crc >> 8);
    return ~crc;
}

} // namespace

// ---------------------------------------------------------------------------
// A template: one function in the source, three in the file.
// ---------------------------------------------------------------------------

template <typename T>
NOINLINE uint32_t checksum(const T *data, size_t n)
{
    uint32_t sum = 0;

    for (size_t i = 0; i < n; i++)
        sum = sum * 31 + static_cast<uint32_t>(data[i]);
    return sum;
}

// ---------------------------------------------------------------------------
// An operator, used by a sort. Its name, operator<, has an unpaired bracket.
// ---------------------------------------------------------------------------

struct Reading {
    uint16_t sensor;
    int16_t value;
};

NOINLINE bool operator<(const Reading &a, const Reading &b)
{
    return a.value < b.value || (a.value == b.value && a.sensor < b.sensor);
}

NOINLINE void sort_readings(Reading *readings, size_t n)
{
    for (size_t i = 1; i < n; i++) {
        Reading r = readings[i];
        size_t j = i;
        while (j > 0 && r < readings[j - 1]) {
            readings[j] = readings[j - 1];
            j--;
        }
        readings[j] = r;
    }
}

// ---------------------------------------------------------------------------
// process_frame, as in the C sample, with a member function inlined into it.
// ---------------------------------------------------------------------------

struct Frame {
    const uint8_t *bytes;
    size_t length;

    ALWAYS_INLINE uint16_t payload_length() const
    {
        return static_cast<uint16_t>(bytes[2] | bytes[3] << 8);
    }
};

const size_t kHeaderSize = 5;
const size_t kCrcSize = 4;
const size_t kMaxSamples = 32;

uint32_t frames_ok;
uint32_t frames_bad;

NOINLINE int process_frame(const Frame &frame, Stage &stage)
{
    if (frame.length < kHeaderSize + kCrcSize ||
        kHeaderSize + frame.payload_length() + kCrcSize > frame.length) {
        frames_bad++;
        log("bad header\n");
        return -1;
    }
    const uint8_t *payload = frame.bytes + kHeaderSize;
    size_t length = frame.payload_length();
    const uint8_t *stored = payload + length;
    uint32_t expected = static_cast<uint32_t>(stored[0] | stored[1] << 8 | stored[2] << 16 |
                                              static_cast<uint32_t>(stored[3]) << 24);
    if (crc32(payload, length) != expected) {
        frames_bad++;
        log("bad crc\n");
        return -1;
    }

    int16_t samples[kMaxSamples];
    size_t n = length / 2 < kMaxSamples ? length / 2 : kMaxSamples;
    for (size_t i = 0; i < n; i++)
        samples[i] = static_cast<int16_t>(payload[2 * i] | payload[2 * i + 1] << 8);
    stage.run(samples, n);
    frames_ok++;
    return 0;
}

// ---------------------------------------------------------------------------
// Lambdas: each is a class of its own, and one that captures nothing becomes
// a plain function when it is stored as a function pointer.
// ---------------------------------------------------------------------------

using Handler = void (*)(uint32_t);

Handler handlers[2];
uint32_t timer_ticks;

NOINLINE void setup_handlers()
{
    handlers[0] = [](uint32_t arg) { timer_ticks += arg; };
    handlers[1] = [](uint32_t arg) {
        log("button ");
        log(arg);
    };
}

NOINLINE void dispatch(unsigned id, uint32_t arg)
{
    if (id < 2 && handlers[id] != nullptr)
        handlers[id](arg);
}

// ---------------------------------------------------------------------------
// A global object with a constructor, which runs from .init_array before
// main.
// ---------------------------------------------------------------------------

class Config {
public:
    Config() { load_defaults(); }
    uint32_t gain() const { return gain_; }

private:
    NOINLINE void load_defaults();
    uint32_t gain_;
};

void Config::load_defaults()
{
    gain_ = 24;
    log("config\n");
}

Config config;

} // namespace app

// Frames with a valid header and CRC, and one with a bad CRC.
static const uint8_t frame_good[] = {
    0xf0, 0xe1, 0x04, 0x00, 0x01, 0x10, 0x00, 0xf0, 0xff, 0xb3, 0x86, 0xe3, 0x48,
};
static const uint8_t frame_bad[] = {
    0xf0, 0xe1, 0x04, 0x00, 0x01, 0x10, 0x00, 0xf0, 0xff, 0x00, 0x00, 0x00, 0x00,
};

int main()
{
    app::Gain gain(static_cast<int16_t>(app::config.gain()));
    app::DcBlock dc_block;

    app::setup_handlers();
    app::process_frame(app::Frame{frame_good, sizeof frame_good}, gain);
    app::process_frame(app::Frame{frame_bad, sizeof frame_bad}, dc_block);
    app::dispatch(0, 10);
    app::dispatch(1, 2);

    app::Reading readings[] = {{1, 30}, {2, -4}, {3, 30}, {4, 7}};
    app::sort_readings(readings, 4);

    uint32_t sum = app::checksum(frame_good, sizeof frame_good) +
                   app::checksum(reinterpret_cast<const uint16_t *>(frame_bad), 6) +
                   app::checksum(reinterpret_cast<const uint32_t *>(frame_good), 3);
    app::log("checksum ");
    app::log(sum);
    app::log(" done\n");
    return app::frames_ok == 1 ? 0 : 1;
}
