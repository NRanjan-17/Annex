// EDID 1.4 block for an Annex virtual monitor. Pure code, unit-tested in tests/EdidTest.cpp.
// SPDX-License-Identifier: MIT
#pragma once

#include <array>
#include <cstdint>

namespace annex {

constexpr uint16_t kEdidProductCode = 0x4558;  // "EX"
constexpr char kEdidManufacturer[4] = "ANX";
constexpr char kMonitorName[] = "Annex Display";

struct EdidSpec {
    uint32_t serial;
    uint32_t width;
    uint32_t height;
    uint32_t refreshHz;
    uint32_t widthMm;
    uint32_t heightMm;
};

std::array<uint8_t, 128> BuildEdid(const EdidSpec& spec);

/// Serial number stored in bytes 12..15 of an EDID block.
uint32_t EdidSerial(const uint8_t* edid, size_t size);

}  // namespace annex
