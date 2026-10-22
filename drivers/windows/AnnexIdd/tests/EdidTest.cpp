// Standalone EDID tests (no driver kit needed). Build + run:
//   cl /std:c++17 /EHsc /W4 /I.. EdidTest.cpp ..\Edid.cpp && EdidTest.exe     (MSVC)
//   c++ -std=c++17 -Wall -I.. EdidTest.cpp ../Edid.cpp -o edid_test && ./edid_test  (clang/gcc)
#include <cstdio>
#include <cstring>
#include <string>

#include "Edid.h"

static int failures = 0;
#define CHECK(cond)                                                    \
    do {                                                               \
        if (!(cond)) {                                                 \
            std::printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            failures++;                                                \
        }                                                              \
    } while (0)

static std::string descriptorText(const std::array<uint8_t, 128>& e, uint8_t tag) {
    for (int at : {54, 72, 90, 108}) {
        if (e[at] == 0 && e[at + 1] == 0 && e[at + 3] == tag) {
            std::string s(reinterpret_cast<const char*>(&e[at + 5]), 13);
            return s.substr(0, s.find('\n'));
        }
    }
    return "";
}

int main() {
    using namespace annex;
    auto e = BuildEdid({0xDEADBEEF, 1920, 1080, 60, 527, 296});
    const uint8_t header[8] = {0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00};
    CHECK(std::memcmp(e.data(), header, 8) == 0);
    uint8_t sum = 0;
    for (uint8_t b : e) sum = static_cast<uint8_t>(sum + b);
    CHECK(sum == 0);

    uint16_t mfg = static_cast<uint16_t>(e[8] << 8 | e[9]);
    char id[4] = {static_cast<char>('@' + ((mfg >> 10) & 31)), static_cast<char>('@' + ((mfg >> 5) & 31)),
                  static_cast<char>('@' + (mfg & 31)), 0};
    CHECK(std::string(id) == "ANX");
    CHECK((e[10] | e[11] << 8) == kEdidProductCode);
    CHECK(EdidSerial(e.data(), e.size()) == 0xDEADBEEF);
    CHECK(e[18] == 1 && e[19] == 4);
    CHECK(e[21] == 53 && e[22] == 30);  // cm

    // Preferred detailed timing decodes back to 1920x1080 and 527x296 mm.
    const uint8_t* d = &e[54];
    CHECK((d[2] | (d[4] >> 4) << 8) == 1920);
    CHECK((d[5] | (d[7] >> 4) << 8) == 1080);
    CHECK((d[12] | (d[14] >> 4) << 8) == 527);
    CHECK((d[13] | (d[14] & 0xF) << 8) == 296);
    CHECK((d[0] | d[1] << 8) > 0);

    CHECK(descriptorText(e, 0xFC) == "Annex Display");
    CHECK(descriptorText(e, 0xFF) == "DEADBEEF");

    // 4K and an odd size still produce valid blocks; physical size falls back to 96 DPI.
    auto big = BuildEdid({1, 3840, 2160, 60, 0, 0});
    sum = 0;
    for (uint8_t b : big) sum = static_cast<uint8_t>(sum + b);
    CHECK(sum == 0);
    CHECK((big[54 + 2] | (big[54 + 4] >> 4) << 8) == 3840);
    CHECK(big[21] == 102);  // 3840 px at 96 DPI = 1016 mm
    CHECK(EdidSerial(nullptr, 0) == 0);

    std::printf(failures ? "%d failure(s)\n" : "all EDID tests passed\n", failures);
    return failures ? 1 : 0;
}
