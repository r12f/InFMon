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
find_package(Threads REQUIRED)
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
    PCAP_DIR=\"${CMAKE_CURRENT_SOURCE_DIR}/../../tests/e2e/scenarios\")
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

# --- Snapshot library (atomic table swap + epoch-based RCU) ---
add_library(infmon_snapshot STATIC
    src/snapshot.c
)
target_include_directories(infmon_snapshot PUBLIC include)
target_link_libraries(infmon_snapshot PUBLIC infmon_counter_table)

add_executable(test_snapshot
    test/test_snapshot.cc
)
target_link_libraries(test_snapshot PRIVATE infmon_snapshot infmon_counter_table GTest::gtest GTest::gtest_main Threads::Threads)
target_include_directories(test_snapshot PRIVATE include)
add_test(NAME snapshot COMMAND test_snapshot)

# --- Stats-segment exposure library ---
add_library(infmon_stats_segment STATIC
    src/stats_segment.c
)
target_include_directories(infmon_stats_segment PUBLIC include)
target_link_libraries(infmon_stats_segment PUBLIC infmon_counter_table)

add_executable(test_stats_segment
    test/test_stats_segment.cc
)
target_link_libraries(test_stats_segment PRIVATE infmon_stats_segment infmon_counter_table GTest::gtest GTest::gtest_main)
target_include_directories(test_stats_segment PRIVATE include)
add_test(NAME stats_segment COMMAND test_stats_segment)

# --- Graph node library (portable processing functions, no VPP dependency) ---
add_library(infmon_graph_node STATIC
    src/graph_node.c
)
target_include_directories(infmon_graph_node PUBLIC include)
target_link_libraries(infmon_graph_node PUBLIC
    infmon_parser
    infmon_flow_rule
    infmon_counter_table
)

add_executable(test_graph_node
    test/test_graph_node.cc
)
target_link_libraries(test_graph_node PRIVATE
    infmon_graph_node
    GTest::gtest GTest::gtest_main
)
target_include_directories(test_graph_node PRIVATE include)
add_test(NAME graph_node COMMAND test_graph_node)

# --- API handler library (flow_rule_add / flow_rule_del orchestration) ---
add_library(infmon_api_handler STATIC
    src/api_handler.c
)
target_include_directories(infmon_api_handler PUBLIC include)
target_link_libraries(infmon_api_handler PUBLIC
    infmon_flow_rule
    infmon_counter_table
    infmon_snapshot
    infmon_stats_segment
)

add_executable(test_api_handler
    test/test_api_handler.cc
)
target_link_libraries(test_api_handler PRIVATE
    infmon_api_handler
    infmon_flow_rule
    infmon_counter_table
    infmon_snapshot
    infmon_stats_segment
    GTest::gtest GTest::gtest_main
)
target_include_directories(test_api_handler PRIVATE include)
add_test(NAME api_handler COMMAND test_api_handler)

# --- VPP API schema (.api -> C header + JSON) ---
list(APPEND CMAKE_MODULE_PATH "${CMAKE_CURRENT_SOURCE_DIR}/cmake")
find_package(VppApiGen)
vpp_generate_api_header(
    API_FILE  "${CMAKE_CURRENT_SOURCE_DIR}/api/infmon.api"
    OUTPUT_DIR "${CMAKE_CURRENT_BINARY_DIR}/generated"
)

# --- API schema validation test (no VPP dependency) ---
add_executable(test_api_schema
    test/test_api_schema.cc
)
target_link_libraries(test_api_schema PRIVATE GTest::gtest GTest::gtest_main)
target_compile_definitions(test_api_schema PRIVATE
    API_FILE=\"${CMAKE_CURRENT_SOURCE_DIR}/api/infmon.api\")
add_test(NAME api_schema COMMAND test_api_schema)

add_executable(test_integration
    test/test_integration.cc
)
target_link_libraries(test_integration PRIVATE
    infmon_api_handler
    infmon_graph_node
    GTest::gtest GTest::gtest_main
    Threads::Threads
)
target_include_directories(test_integration PRIVATE include)
add_test(NAME integration COMMAND test_integration)

# --- VPP plugin shared library (.so) ---
option(ENABLE_VPP_PLUGIN "Build infmon_plugin.so for VPP runtime loading" OFF)

if(ENABLE_VPP_PLUGIN)
    # Locate VPP development headers and libraries
    find_path(VPP_INCLUDE_DIR vlib/vlib.h
        PATHS /usr/include /usr/local/include
        DOC "VPP development headers"
    )
    find_library(VPP_LIB_VLIB NAMES vlib DOC "VPP vlib library")
    find_library(VPP_LIB_VNET NAMES vnet DOC "VPP vnet library")
    find_library(VPP_LIB_VLIBMEMORY NAMES vlibmemory DOC "VPP vlibmemory library")
    find_library(VPP_LIB_VPPINFRA NAMES vppinfra DOC "VPP vppinfra library")

    if(NOT VPP_INCLUDE_DIR)
        message(FATAL_ERROR "ENABLE_VPP_PLUGIN is ON but VPP dev headers not found. Install vpp-dev.")
    endif()
    if(NOT VPP_LIB_VLIB OR NOT VPP_LIB_VNET)
        message(FATAL_ERROR "ENABLE_VPP_PLUGIN is ON but required VPP libraries (vlib, vnet) not found.")
    endif()

    add_library(infmon_plugin MODULE
        src/vpp/infmon_nodes.c
    )
    set_target_properties(infmon_plugin PROPERTIES
        PREFIX ""                              # produce infmon_plugin.so, not libinfmon_plugin.so
        OUTPUT_NAME "infmon_plugin"
        C_VISIBILITY_PRESET default
    )
    target_compile_definitions(infmon_plugin PRIVATE INFMON_VPP_BUILD)
    target_include_directories(infmon_plugin PRIVATE
        include
        ${VPP_INCLUDE_DIR}
        ${CMAKE_CURRENT_BINARY_DIR}/generated  # for generated API header
    )
    target_link_libraries(infmon_plugin PRIVATE
        infmon_graph_node
        infmon_parser
        infmon_flow_rule
        infmon_counter_table
        infmon_snapshot
        infmon_stats_segment
    )
    foreach(_lib ${VPP_LIB_VLIB} ${VPP_LIB_VNET} ${VPP_LIB_VLIBMEMORY} ${VPP_LIB_VPPINFRA})
        target_link_libraries(infmon_plugin PRIVATE ${_lib})
    endforeach()

    # Ensure the generated API header is built before the plugin
    add_dependencies(infmon_plugin infmon_api_generated)

    # Install to VPP plugin directory
    include(GNUInstallDirs)
    install(TARGETS infmon_plugin
        LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}/vpp_plugins
    )

    message(STATUS "VPP plugin target enabled: infmon_plugin.so")
endif()

if(ENABLE_FUZZER)
    add_executable(fuzz_erspan_parser
        test/fuzz_erspan_parser.cc
    )
    target_link_libraries(fuzz_erspan_parser PRIVATE infmon_parser)
    target_include_directories(fuzz_erspan_parser PRIVATE include)
    target_compile_options(fuzz_erspan_parser PRIVATE -fsanitize=fuzzer,address,undefined)
    target_link_options(fuzz_erspan_parser PRIVATE -fsanitize=fuzzer,address,undefined)
endif()
