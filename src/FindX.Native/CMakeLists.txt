cmake_minimum_required(VERSION 3.20)
project(FindXNative LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)

add_library(FindXNative SHARED FindXNative.cpp)
target_compile_definitions(FindXNative PRIVATE FINDX_EXPORTS UNICODE _UNICODE)

if(MSVC)
    target_compile_options(FindXNative PRIVATE /W4 /O2)
endif()
