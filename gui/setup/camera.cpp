#include "camera.h"

#include "kface_sensor.hpp"

#include <QDir>
#include <QFile>
#include <QMetaObject>
#include <QProcess>
#include <QStandardPaths>

#include <pwd.h>
#include <unistd.h>

#include <chrono>

using namespace kface;
using Clock = std::chrono::steady_clock;

static constexpr int ENROLL_SAMPLES = 20;
static constexpr double ENROLL_TIMEOUT_S = 60, TEST_TIMEOUT_S = 8;
static const char *KFACE_BIN = "/usr/local/bin/kface";

QImage FrameProvider::requestImage(const QString &, QSize *size, const QSize &)
{
    QMutexLocker l(&mutex_);
    if (size) *size = image_.size();
    return image_;
}

void FrameProvider::set(const QImage &img)
{
    QMutexLocker l(&mutex_);
    image_ = img;
}

Camera::Camera(FrameProvider *frames, QObject *parent) : QObject(parent), frames_(frames)
{
    if (struct passwd *pw = getpwuid(getuid())) user_ = QString::fromLocal8Bit(pw->pw_name);
    reloadEnrolled();
    start();
}

Camera::~Camera() { stop(); }

void Camera::start()
{
    stop_ = false;
    status_ = "starting";
    emit changed();
    worker_ = std::thread([this] { run(); });
}

void Camera::stop()
{
    stop_ = true;
    if (worker_.joinable()) worker_.join();
}

void Camera::reloadEnrolled()
{
    enrolled_ = (int)load_faces(user_.toStdString()).size();
    emit changed();
}

void Camera::setMode(Mode m)
{
    generation_++;
    mode_req_ = m;
    mode_ = m == Enroll ? "enroll" : m == Test ? "test" : "preview";
    progress_ = 0;
    emit changed();
}

void Camera::startEnroll() { setMode(Enroll); }
void Camera::startTest() { setMode(Test); }
void Camera::cancel() { setMode(Preview); }

// The worker owns the Kinect. All results are posted back to the GUI thread.
void Camera::run()
{
    auto post = [this](auto fn) { QMetaObject::invokeMethod(this, fn, Qt::QueuedConnection); };

    Config cfg;
    load_config(cfg);
    FaceEngine fe;
    if (!fe.init()) {
        post([this] { status_ = "nomodels"; emit changed(); });
        return;
    }
    Kinect k;
    if (!k.open()) {
        post([this] { status_ = "nodevice"; emit changed(); });
        return;
    }
    post([this] { status_ = "ready"; emit changed(); });

    std::vector<cv::Mat> enrolled = load_faces(user_.toStdString()), samples;
    int gen = -1;
    Streak streak;
    auto t0 = Clock::now();
    cv::Mat ir, depth;

    while (!stop_) {
        if (!k.grab(ir, depth)) continue;

        // mode changes from the GUI reset per-mode state
        if (gen != generation_) {
            gen = generation_;
            samples.clear();
            streak = {};
            t0 = Clock::now();
            enrolled = load_faces(user_.toStdString());
        }
        int mode = mode_req_;
        double elapsed = std::chrono::duration<double>(Clock::now() - t0).count();

        Face f;
        bool seen = fe.analyse(ir, depth, k, cfg, f);
        float sim = 0;
        if (seen)
            for (auto &m : enrolled) sim = std::max(sim, fe.match(m, f.feature));

        double progress = 0;
        QString done, msg;
        bool ok = false;
        if (mode == Enroll) {
            if (seen && f.live) {
                bool dup = false;
                for (auto &m : samples) dup |= fe.match(m, f.feature) > 0.92f;
                if (!dup) samples.push_back(f.feature.clone());
            }
            progress = (double)samples.size() / ENROLL_SAMPLES;
            if ((int)samples.size() >= ENROLL_SAMPLES) {
                QString path = QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation) + "/kface-enroll.faces";
                QFile::remove(path);
                if (write_faces(path.toStdString(), samples)) {
                    QFile::setPermissions(path, QFile::ReadOwner | QFile::WriteOwner);
                    post([this, path] { install(path); });
                    return;  // install() releases and restarts the camera
                }
                done = "enroll", msg = "Could not write temporary file";
            } else if (elapsed > ENROLL_TIMEOUT_S) {
                done = "enroll", msg = "Timed out. Make sure your face is well visible.";
            }
        } else if (mode == Test) {
            if (enrolled.empty()) {
                done = "test", msg = "No face set up yet";
            } else {
                bool good = seen && f.live && sim >= cfg.match_threshold && facing_sensor(f, cfg);
                progress = elapsed / TEST_TIMEOUT_S;
                if (streak.update(elapsed, good, cfg.attention_s)) done = "test", ok = true, msg = QString("Recognized (similarity %1)").arg(sim, 0, 'f', 2);
                else if (elapsed > TEST_TIMEOUT_S) done = "test", msg = "Not recognized";
            }
        }

        QImage img(ir.data, ir.cols, ir.rows, (int)ir.step, QImage::Format_Grayscale8);
        frames_->set(img.copy());

        QRectF rect;
        if (seen) {
            const float *d = f.det.ptr<float>();
            rect = QRectF(d[0] / FaceEngine::W, d[1] / FaceEngine::H, d[2] / FaceEngine::W, d[3] / FaceEngine::H);
        }
        QString why = QString::fromStdString(f.why);
        bool live = f.live, facing = seen && facing_sensor(f, cfg);
        post([=, this] {
            frame_++;
            faceVisible_ = seen;
            faceRect_ = rect;
            live_ = live;
            facing_ = facing;
            why_ = why;
            similarity_ = sim;
            if (mode_req_ == mode) progress_ = std::min(1.0, progress);
            emit changed();
            if (!done.isEmpty() && mode_req_ == mode) {
                setMode(Preview);
                emit finished(done, ok, msg);
            }
        });
    }
}

// Hand the captured samples to root via polkit. The Kinect is released first so that the
// polkit prompt itself can use face authentication.
void Camera::install(const QString &path)
{
    stop();
    mode_ = "saving";
    status_ = "paused";
    emit changed();
    auto *p = new QProcess(this);
    connect(p, &QProcess::finished, this, [this, p, path](int code, QProcess::ExitStatus st) {
        QFile::remove(path);
        bool ok = st == QProcess::NormalExit && code == 0;
        QString err = QString::fromLocal8Bit(p->readAllStandardError()).trimmed();
        p->deleteLater();
        setMode(Preview);
        reloadEnrolled();
        emit finished("enroll", ok, ok ? "Face set up" : (err.isEmpty() ? "Authorization failed" : err));
        start();
    });
    p->start("pkexec", {KFACE_BIN, "import", path});
}

void Camera::removeFaces()
{
    stop();
    status_ = "paused";
    emit changed();
    auto *p = new QProcess(this);
    connect(p, &QProcess::finished, this, [this, p](int code, QProcess::ExitStatus st) {
        bool ok = st == QProcess::NormalExit && code == 0;
        p->deleteLater();
        reloadEnrolled();
        emit finished("remove", ok, ok ? "Face data removed" : "Authorization failed");
        start();
    });
    p->start("pkexec", {KFACE_BIN, "remove"});
}
