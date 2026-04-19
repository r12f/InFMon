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
