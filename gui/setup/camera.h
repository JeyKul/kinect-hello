// Camera: runs the Kinect + face engine on a worker thread and exposes live state to QML.
#pragma once

#include <QImage>
#include <QMutex>
#include <QObject>
#include <QQuickImageProvider>
#include <QRectF>

#include <atomic>
#include <thread>

class FrameProvider : public QQuickImageProvider {
public:
    FrameProvider() : QQuickImageProvider(QQuickImageProvider::Image) {}
    QImage requestImage(const QString &, QSize *size, const QSize &) override;
    void set(const QImage &img);

private:
    QMutex mutex_;
    QImage image_;
};

class Camera : public QObject {
    Q_OBJECT
    Q_PROPERTY(QString status MEMBER status_ NOTIFY changed)      // starting, ready, nodevice, nomodels, paused
    Q_PROPERTY(QString mode MEMBER mode_ NOTIFY changed)          // preview, enroll, test, saving
    Q_PROPERTY(int frame MEMBER frame_ NOTIFY changed)
    Q_PROPERTY(bool faceVisible MEMBER faceVisible_ NOTIFY changed)
    Q_PROPERTY(QRectF faceRect MEMBER faceRect_ NOTIFY changed)  // normalised to the image
    Q_PROPERTY(bool live MEMBER live_ NOTIFY changed)
    Q_PROPERTY(QString why MEMBER why_ NOTIFY changed)
    Q_PROPERTY(double similarity MEMBER similarity_ NOTIFY changed)
    Q_PROPERTY(double progress MEMBER progress_ NOTIFY changed)
    Q_PROPERTY(int enrolled MEMBER enrolled_ NOTIFY changed)
    Q_PROPERTY(QString user MEMBER user_ CONSTANT)

public:
    explicit Camera(FrameProvider *frames, QObject *parent = nullptr);
    ~Camera() override;

    Q_INVOKABLE void startEnroll();
    Q_INVOKABLE void startTest();
    Q_INVOKABLE void cancel();
    Q_INVOKABLE void removeFaces();

signals:
    void changed();
    void finished(const QString &what, bool ok, const QString &message);

private:
    enum Mode { Preview, Enroll, Test };
    void start();
    void stop();
    void run();
    void reloadEnrolled();
    void install(const QString &path);
    void setMode(Mode m);

    FrameProvider *frames_;
    std::thread worker_;
    std::atomic<bool> stop_{false};
    std::atomic<int> mode_req_{Preview};
    std::atomic<int> generation_{0};

    QString status_ = "starting", mode_ = "preview", why_, user_;
    int frame_ = 0, enrolled_ = 0;
    bool faceVisible_ = false, live_ = false;
    QRectF faceRect_;
    double similarity_ = 0, progress_ = 0;
};
