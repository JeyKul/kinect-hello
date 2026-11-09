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

enable_testing()
add_executable(kface_tests tests/test_core.cpp)
target_link_libraries(kface_tests PRIVATE opencv_core)
add_test(NAME core COMMAND kface_tests)

# ---------------------------------------------------------------- GUI
option(KFACE_GUI "Build kface-setup and kface-overlay" ON)
if(KFACE_GUI)
  find_package(Qt6 6.5 REQUIRED COMPONENTS Gui Quick Network)
  find_package(LayerShellQt REQUIRED)
  qt_standard_project_setup(REQUIRES 6.5)

  # all QML lives at each module's root so Main.qml sees the shared FaceGlyph
  set_source_files_properties(gui/FaceGlyph.qml PROPERTIES QT_RESOURCE_ALIAS FaceGlyph.qml)
  set_source_files_properties(gui/setup/Main.qml gui/overlay/Main.qml PROPERTIES QT_RESOURCE_ALIAS Main.qml)
  qt_policy(SET QTP0004 NEW)

  qt_add_executable(kface-setup gui/setup/main.cpp gui/setup/camera.cpp gui/setup/camera.h)
  qt_add_qml_module(kface-setup URI KFaceSetup VERSION 1.0
    QML_FILES gui/setup/Main.qml gui/FaceGlyph.qml)
  target_include_directories(kface-setup PRIVATE src ${freenect2_INCLUDE_DIRS})
  target_link_libraries(kface-setup PRIVATE Qt6::Quick ${freenect2_LIBRARIES} ${OpenCV_LIBS})
  set_target_properties(kface-setup PROPERTIES BUILD_RPATH "${freenect2_DIR}/../.." INSTALL_RPATH "/usr/local/lib/kface")

  qt_add_executable(kface-overlay gui/overlay/main.cpp)
  qt_add_qml_module(kface-overlay URI KFaceOverlay VERSION 1.0
    QML_FILES gui/overlay/Main.qml gui/FaceGlyph.qml)
  target_include_directories(kface-overlay PRIVATE src)
  target_link_libraries(kface-overlay PRIVATE Qt6::Quick Qt6::Network LayerShellQt::Interface)

  install(TARGETS kface-setup kface-overlay DESTINATION bin)
  install(FILES gui/kface-setup.desktop DESTINATION share/applications)
  install(FILES gui/kface-overlay.desktop DESTINATION /etc/xdg/autostart)
endif()
