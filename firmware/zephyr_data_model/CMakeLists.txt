cmake_minimum_required(VERSION 3.20.0)
find_package(Zephyr REQUIRED HINTS $ENV{ZEPHYR_BASE})
project(zephyr_data_model)

target_sources(app PRIVATE src/main.c ../data_model/data_model.c)
target_include_directories(app PRIVATE ../data_model)
