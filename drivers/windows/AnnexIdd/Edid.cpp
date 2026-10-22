// SPDX-License-Identifier: MIT
#include "Edid.h"

#include <algorithm>
#include <cstdio>
#include <cstring>

namespace annex {

std::array<uint8_t, 128> BuildEdid(const EdidSpec& s) {
    std::array<uint8_t, 128> e{};
    const uint8_t header[8] = {0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00};
    std::memcpy(e.data(), header, 8);

    // Manufacturer ID: three 5-bit letters, big-endian.
    const uint16_t mfg = static_cast<uint16_t>(((kEdidManufacturer[0] - '@') << 10) |
                                               ((kEdidManufacturer[1] - '@') << 5) | (kEdidManufacturer[2] - '@'));
    e[8] = static_cast<uint8_t>(mfg >> 8);
    e[9] = static_cast<uint8_t>(mfg & 0xFF);
    e[10] = kEdidProductCode & 0xFF;
    e[11] = kEdidProductCode >> 8;
    for (int i = 0; i < 4; i++) e[12 + i] = static_cast<uint8_t>(s.serial >> (8 * i));
    e[16] = 0;    // week
    e[17] = 36;   // 2026
    e[18] = 1;    // EDID 1.4
    e[19] = 4;
    e[20] = 0xA0;  // digital, 8 bits per colour

    const uint32_t widthMm = s.widthMm ? s.widthMm : s.width * 254 / 960;  // 96 DPI fallback
    const uint32_t heightMm = s.heightMm ? s.heightMm : s.height * 254 / 960;
    e[21] = static_cast<uint8_t>(std::min<uint32_t>(255, (widthMm + 5) / 10));
    e[22] = static_cast<uint8_t>(std::min<uint32_t>(255, (heightMm + 5) / 10));
    e[23] = 120;   // gamma 2.2
    e[24] = 0x06;  // sRGB default, preferred timing is native
    const uint8_t srgb[10] = {0xEE, 0x91, 0xA3, 0x54, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54};
    std::memcpy(&e[25], srgb, 10);
    // 35..37 established timings: none. 38..53 standard timings: unused.
    for (int i = 38; i < 54; i++) e[i] = 0x01;

    // Descriptor 1: detailed timing for the preferred mode (reduced blanking).
    uint8_t* d = &e[54];
    const uint32_t hBlank = 160, vBlank = std::max<uint32_t>(25, s.height / 30);
    const uint32_t hz = s.refreshHz ? s.refreshHz : 60;
    uint64_t clock = static_cast<uint64_t>(s.width + hBlank) * (s.height + vBlank) * hz / 10000;  // 10 kHz units
    clock = std::min<uint64_t>(clock, 0xFFFF);  // larger modes are still offered via IddCx mode lists
    d[0] = static_cast<uint8_t>(clock & 0xFF);
    d[1] = static_cast<uint8_t>(clock >> 8);
    d[2] = s.width & 0xFF;
    d[3] = hBlank & 0xFF;
    d[4] = static_cast<uint8_t>(((s.width >> 8) & 0xF) << 4 | ((hBlank >> 8) & 0xF));
    d[5] = s.height & 0xFF;
    d[6] = static_cast<uint8_t>(vBlank & 0xFF);
    d[7] = static_cast<uint8_t>(((s.height >> 8) & 0xF) << 4 | ((vBlank >> 8) & 0xF));
    const uint32_t hFront = 48, hSync = 32, vFront = 3, vSync = 5;
    d[8] = hFront & 0xFF;
    d[9] = hSync & 0xFF;
    d[10] = static_cast<uint8_t>((vFront & 0xF) << 4 | (vSync & 0xF));
    d[11] = 0;
    d[12] = widthMm & 0xFF;
    d[13] = heightMm & 0xFF;
    d[14] = static_cast<uint8_t>(((widthMm >> 8) & 0xF) << 4 | ((heightMm >> 8) & 0xF));
    d[15] = 0;
    d[16] = 0;
    d[17] = 0x1A;  // digital separate sync, +h +v

    auto text = [&](int at, uint8_t tag, const char* str) {
        uint8_t* t = &e[at];
        t[0] = t[1] = t[2] = 0;
        t[3] = tag;
        t[4] = 0;
        size_t n = std::min<size_t>(std::strlen(str), 13);
        std::memcpy(&t[5], str, n);
        if (n < 13) {
            t[5 + n] = 0x0A;
            for (size_t i = n + 1; i < 13; i++) t[5 + i] = 0x20;
        }
    };
    text(72, 0xFC, kMonitorName);

    // Descriptor 3: range limits.
    uint8_t* r = &e[90];
    r[3] = 0xFD;
    r[5] = 24;   // min vertical Hz
    r[6] = 240;  // max vertical Hz
    r[7] = 1;    // min horizontal kHz
    r[8] = 255;  // max horizontal kHz
    r[9] = 255;  // max pixel clock / 10 MHz
    r[10] = 0x01;
    r[11] = 0x0A;
    for (int i = 12; i < 18; i++) r[i] = 0x20;

    char serial[16];
    std::snprintf(serial, sizeof serial, "%08X", s.serial);
    text(108, 0xFF, serial);

    e[126] = 0;  // no extensions
    uint8_t sum = 0;
    for (int i = 0; i < 127; i++) sum = static_cast<uint8_t>(sum + e[i]);
    e[127] = static_cast<uint8_t>(0x100 - sum);
    return e;
}

uint32_t EdidSerial(const uint8_t* edid, size_t size) {
    if (!edid || size < 16) return 0;
    return static_cast<uint32_t>(edid[12]) | static_cast<uint32_t>(edid[13]) << 8 | static_cast<uint32_t>(edid[14]) << 16 |
           static_cast<uint32_t>(edid[15]) << 24;
}

}  // namespace annex
