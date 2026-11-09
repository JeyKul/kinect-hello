cmake_minimum_required(VERSION 3.16)
project(kface CXX)
set(CMAKE_CXX_STANDARD 17)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

find_package(freenect2 REQUIRED)
find_package(OpenCV REQUIRED COMPONENTS core imgproc imgcodecs objdetect dnn)

add_executable(kface src/kface.cpp)
target_include_directories(kface PRIVATE ${freenect2_INCLUDE_DIRS})
target_link_libraries(kface PRIVATE ${freenect2_LIBRARIES} ${OpenCV_LIBS})
set_target_properties(kface PROPERTIES BUILD_RPATH "${freenect2_DIR}/../.." INSTALL_RPATH "/usr/local/lib/kface")

install(TARGETS kface DESTINATION bin)
