cmake_minimum_required(VERSION 3.16)
project(infmon-backend C CXX)

set(CMAKE_C_STANDARD 11)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

# --- Compiler flags ---
add_compile_options(-Wall -Wextra -Wpedantic -Werror)

# --- ERSPAN parser library (pure C, no VPP dependency) ---
add_library(infmon_parser STATIC
    src/erspan_parser.c
)
target_include_directories(infmon_parser PUBLIC include)

# --- Tests ---
find_package(GTest REQUIRED)
enable_testing()

add_executable(test_erspan_parser
    test/test_erspan_parser.cc
)
target_link_libraries(test_erspan_parser PRIVATE infmon_parser GTest::gtest GTest::gtest_main)
target_include_directories(test_erspan_parser PRIVATE include)
add_test(NAME erspan_parser COMMAND test_erspan_parser)

# --- Golden PCAP tests ---
add_executable(test_erspan_pcap
    test/test_erspan_pcap.cc
)
target_link_libraries(test_erspan_pcap PRIVATE infmon_parser GTest::gtest GTest::gtest_main)
target_include_directories(test_erspan_pcap PRIVATE include)
target_compile_definitions(test_erspan_pcap PRIVATE
    PCAP_DIR=\"${CMAKE_CURRENT_SOURCE_DIR}/../../tests/pcaps/erspan\")
add_test(NAME erspan_pcap COMMAND test_erspan_pcap)

# --- Fuzz target (clang only, requires -fsanitize=fuzzer) ---
option(ENABLE_FUZZER "Build libFuzzer targets (requires clang)" OFF)
# --- Flow-rule data model library ---
add_library(infmon_flow_rule STATIC
    src/flow_rule.c
)
target_include_directories(infmon_flow_rule PUBLIC include)

add_executable(test_flow_rule
    test/test_flow_rule.cc
)
target_link_libraries(test_flow_rule PRIVATE infmon_flow_rule GTest::gtest GTest::gtest_main)
target_include_directories(test_flow_rule PRIVATE include)
add_test(NAME flow_rule COMMAND test_flow_rule)

# --- Counter table library ---
add_library(infmon_counter_table STATIC
    src/counter_table.c
)
target_include_directories(infmon_counter_table PUBLIC include)

add_executable(test_counter_table
    test/test_counter_table.cc
)
target_link_libraries(test_counter_table PRIVATE infmon_counter_table GTest::gtest GTest::gtest_main)
target_include_directories(test_counter_table PRIVATE include)
add_test(NAME counter_table COMMAND test_counter_table)

# --- Graph node library ---
add_library(infmon_graph_node STATIC
    src/graph_node.c
)
target_include_directories(infmon_graph_node PUBLIC include)
target_link_libraries(infmon_graph_node PUBLIC infmon_parser infmon_flow_rule infmon_counter_table)

add_executable(test_graph_node
    test/test_graph_node.cc
)
target_link_libraries(test_graph_node PRIVATE infmon_graph_node GTest::gtest GTest::gtest_main)
target_include_directories(test_graph_node PRIVATE include)
add_test(NAME graph_node COMMAND test_graph_node)

if(ENABLE_FUZZER)
    add_executable(fuzz_erspan_parser
        test/fuzz_erspan_parser.cc
    )
    target_link_libraries(fuzz_erspan_parser PRIVATE infmon_parser)
    target_include_directories(fuzz_erspan_parser PRIVATE include)
    target_compile_options(fuzz_erspan_parser PRIVATE -fsanitize=fuzzer,address,undefined)
    target_link_options(fuzz_erspan_parser PRIVATE -fsanitize=fuzzer,address,undefined)
endif()
