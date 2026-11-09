// kface-setup: enroll, test and remove your face for Kinect face unlock
#include "camera.h"

#include <QGuiApplication>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQmlContext>

#include <opencv2/core/utils/logger.hpp>

int main(int argc, char **argv)
{
    cv::utils::logging::setLogLevel(cv::utils::logging::LOG_LEVEL_SILENT);
    QGuiApplication app(argc, argv);
    app.setApplicationName("kface-setup");
    app.setApplicationDisplayName("Face Unlock");
    app.setDesktopFileName("kface-setup");
    app.setWindowIcon(QIcon::fromTheme("user-identity"));

    QQmlApplicationEngine engine;
    auto *frames = new FrameProvider;  // owned by the engine
    engine.addImageProvider("kinect", frames);
    Camera camera(frames);
    engine.rootContext()->setContextProperty("camera", &camera);
    engine.loadFromModule("KFaceSetup", "Main");
    if (engine.rootObjects().isEmpty()) return 1;
    return app.exec();
}
