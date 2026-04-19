# FindVppApiGen.cmake — Locate vppapigen and provide a function to
# generate C headers from .api files.
#
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Riff
#
# Usage:
#   find_package(VppApiGen)         # optional — VPPAPIGEN_FOUND is set
#   if(VPPAPIGEN_FOUND)
#     vpp_generate_api_header(
#       API_FILE  src/backend/api/infmon.api
#       OUTPUT_DIR ${CMAKE_CURRENT_BINARY_DIR}/generated
#     )
#   endif()
#
# The generated header is: ${OUTPUT_DIR}/<basename>.api.h
# A CMake target "${basename}_api_generated" is created that depends on it.

# Try to find vppapigen — shipped with vpp-dev
find_program(VPPAPIGEN_EXECUTABLE
    NAMES vppapigen
    PATHS /usr/bin /usr/local/bin
    DOC "VPP API generator (vppapigen)"
)

include(FindPackageHandleStandardArgs)
find_package_handle_standard_args(VppApiGen
    REQUIRED_VARS VPPAPIGEN_EXECUTABLE
)

if(VPPAPIGEN_FOUND)
    message(STATUS "Found vppapigen: ${VPPAPIGEN_EXECUTABLE}")
endif()

function(vpp_generate_api_header)
    cmake_parse_arguments(ARG "" "API_FILE;OUTPUT_DIR" "" ${ARGN})

    if(NOT ARG_API_FILE)
        message(FATAL_ERROR "vpp_generate_api_header: API_FILE is required")
    endif()
    if(NOT ARG_OUTPUT_DIR)
        message(FATAL_ERROR "vpp_generate_api_header: OUTPUT_DIR is required")
    endif()

    get_filename_component(API_BASENAME "${ARG_API_FILE}" NAME)
    set(OUTPUT_HEADER "${ARG_OUTPUT_DIR}/${API_BASENAME}.h")
    set(OUTPUT_JSON "${ARG_OUTPUT_DIR}/${API_BASENAME}.json")

    file(MAKE_DIRECTORY "${ARG_OUTPUT_DIR}")

    if(VPPAPIGEN_FOUND)
        # Generate C header
        add_custom_command(
            OUTPUT "${OUTPUT_HEADER}"
            COMMAND ${VPPAPIGEN_EXECUTABLE}
                --input "${ARG_API_FILE}"
                --output "${OUTPUT_HEADER}"
                --outputdir "${ARG_OUTPUT_DIR}"
            DEPENDS "${ARG_API_FILE}"
            COMMENT "Generating VPP API header from ${API_BASENAME}"
            VERBATIM
        )

        # Generate JSON (for Rust bindings / validation)
        add_custom_command(
            OUTPUT "${OUTPUT_JSON}"
            COMMAND ${VPPAPIGEN_EXECUTABLE}
                --input "${ARG_API_FILE}"
                --output "${OUTPUT_JSON}"
                --outputdir "${ARG_OUTPUT_DIR}"
                JSON
            DEPENDS "${ARG_API_FILE}"
            COMMENT "Generating VPP API JSON from ${API_BASENAME}"
            VERBATIM
        )

        get_filename_component(_api_stem "${ARG_API_FILE}" NAME_WE)
        add_custom_target(${_api_stem}_api_generated
            DEPENDS "${OUTPUT_HEADER}" "${OUTPUT_JSON}"
        )
    else()
        message(STATUS "vppapigen not found — API header generation skipped (pre-generated headers required)")
        # Create a dummy target so dependents don't break
        get_filename_component(_api_stem "${ARG_API_FILE}" NAME_WE)
        add_custom_target(${_api_stem}_api_generated)
    endif()

    # Export variables to parent scope
    set(INFMON_API_HEADER "${OUTPUT_HEADER}" PARENT_SCOPE)
    set(INFMON_API_JSON "${OUTPUT_JSON}" PARENT_SCOPE)
    set(INFMON_API_OUTPUT_DIR "${ARG_OUTPUT_DIR}" PARENT_SCOPE)
endfunction()
