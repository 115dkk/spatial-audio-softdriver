# SPDX-License-Identifier: MS-PL
# User-mode builds and checks for the allocation-free driver core.
cmake_minimum_required(VERSION 3.20)
project(bio_driver_core LANGUAGES C CXX)

option(BIO_SANITIZE "Enable address and undefined-behavior sanitizers" OFF)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

add_library(bio_core STATIC bio_ring.cpp bio_read.cpp bio_clock.cpp bio_u128.cpp bio_format.cpp)
target_include_directories(bio_core PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${CMAKE_CURRENT_SOURCE_DIR}/../../crates/bio-abi/include)

add_executable(core_tests tests/core_tests.cpp)
target_link_libraries(core_tests PRIVATE bio_core)
add_executable(write_dump tests/write_dump.cpp)
target_link_libraries(write_dump PRIVATE bio_core)

if(MSVC)
    set(BIO_WARNINGS /W4 /WX /permissive- /EHs-c- /GR-)
    target_compile_options(bio_core PRIVATE ${BIO_WARNINGS})
    target_compile_options(core_tests PRIVATE ${BIO_WARNINGS})
    target_compile_options(write_dump PRIVATE ${BIO_WARNINGS})
    # fopen is C4996 under MSVC; the tool writes a file and nothing else.
    target_compile_definitions(write_dump PRIVATE _CRT_SECURE_NO_WARNINGS)
    if(BIO_SANITIZE)
        target_compile_options(bio_core PRIVATE /fsanitize=address)
        target_compile_options(core_tests PRIVATE /fsanitize=address)
        target_compile_options(write_dump PRIVATE /fsanitize=address)
    endif()
else()
    set(BIO_WARNINGS -Wall -Wextra -Wpedantic -Wconversion -Wsign-conversion -Werror
        -fno-exceptions -fno-rtti)
    target_compile_options(bio_core PRIVATE ${BIO_WARNINGS})
    target_compile_options(core_tests PRIVATE ${BIO_WARNINGS})
    target_compile_options(write_dump PRIVATE ${BIO_WARNINGS})
    if(BIO_SANITIZE)
        set(BIO_SANITIZERS -fsanitize=address,undefined -fno-omit-frame-pointer)
        target_compile_options(bio_core PRIVATE ${BIO_SANITIZERS})
        target_compile_options(core_tests PRIVATE ${BIO_SANITIZERS})
        target_compile_options(write_dump PRIVATE ${BIO_SANITIZERS})
        target_link_options(core_tests PRIVATE -fsanitize=address,undefined)
        target_link_options(write_dump PRIVATE -fsanitize=address,undefined)
    endif()
endif()

if(CMAKE_CXX_COMPILER_ID MATCHES "Clang")
    add_executable(ring_fuzz fuzz/ring_fuzz.cpp)
    target_link_libraries(ring_fuzz PRIVATE bio_core)
    target_compile_options(ring_fuzz PRIVATE ${BIO_WARNINGS} -fsanitize=fuzzer,address,undefined
        -fno-omit-frame-pointer)
    target_link_options(ring_fuzz PRIVATE -fsanitize=fuzzer,address,undefined)
    # Compile the classifier into this target so libFuzzer sees its branches,
    # not just the harness calls into an uninstrumented static library.
    add_executable(format_fuzz fuzz/format_fuzz.cpp bio_format.cpp)
    target_include_directories(format_fuzz PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}
        ${CMAKE_CURRENT_SOURCE_DIR}/../../crates/bio-abi/include)
    target_compile_options(format_fuzz PRIVATE ${BIO_WARNINGS} -fsanitize=fuzzer,address,undefined
        -fno-omit-frame-pointer)
    target_link_options(format_fuzz PRIVATE -fsanitize=fuzzer,address,undefined)
    add_executable(clock_fuzz fuzz/clock_fuzz.cpp)
    target_link_libraries(clock_fuzz PRIVATE bio_core)
    target_compile_options(clock_fuzz PRIVATE ${BIO_WARNINGS} -fsanitize=fuzzer,address,undefined
        -fno-omit-frame-pointer)
    target_link_options(clock_fuzz PRIVATE -fsanitize=fuzzer,address,undefined)
endif()
