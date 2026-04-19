// SPDX-License-Identifier: Apache-2.0
// libFuzzer harness for the ERSPAN III parser.
// See specs/003-erspan-and-packet-parsing.md §8.3

#include <cstddef>
#include <cstdint>

extern "C" {
#include "infmon/erspan_parser.h"
}

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size)
{
    // Cap at 2048 bytes per spec recommendation
    if (size > 2048)
        return 0;

    infmon_parsed_packet_t out;
    infmon_parse_erspan(data, (uint32_t)size, &out);
    return 0;
}
