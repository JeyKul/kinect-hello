// kface - Kinect v2 IR + depth face authentication
//
//   kface debug  [n]        save n annotated IR frames to ./debug, print metrics
//   kface enroll <user>     capture and store IR face embeddings for <user>
//   kface verify <user>     exit 0 if a live, matching face is seen within timeout
//
// Recognition runs on the IR image (works in darkness, screens are black in IR).
// Liveness uses the pixel-aligned depth map: the face must have real 3D relief
// (nose in front of eyes, non-planar surface) and a metric size of a human face.

#include <libfreenect2/libfreenect2.hpp>
#include <libfreenect2/frame_listener_impl.h>
#include <libfreenect2/packet_pipeline.h>
#include <libfreenect2/logger.h>
#include <opencv2/opencv.hpp>
#include <opencv2/objdetect/face.hpp>
#include <opencv2/core/utils/logger.hpp>

#include "kface_core.hpp"

#include <pwd.h>
#include <unistd.h>

#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

using namespace kface;
using Clock = std::chrono::steady_clock;

// ---------------------------------------------------------------- sensor

class Kinect {
public:
    bool open()
    {
        libfreenect2::setGlobalLogger(libfreenect2::createConsoleLogger(
            getenv("KFACE_VERBOSE") ? libfreenect2::Logger::Info : libfreenect2::Logger::None));
        if (fn_.enumerateDevices() == 0) {
            fprintf(stderr, "kface: no Kinect v2 found\n");
            return false;
        }
        dev_ = fn_.openDefaultDevice(new libfreenect2::CpuPacketPipeline());
        if (!dev_) return false;
        dev_->setIrAndDepthFrameListener(&listener_);
        if (!dev_->startStreams(false, true)) return false;
        auto p = dev_->getIrCameraParams();
        fx_ = p.fx; fy_ = p.fy; cx_ = p.cx; cy_ = p.cy;
        return true;
    }

    ~Kinect()
    {
        if (dev_) {
            dev_->stop();
            dev_->close();
            delete dev_;
        }
    }

    // ir: 8-bit contrast-normalised, depth: float mm (0 = invalid); both mirrored to selfie view
    bool grab(cv::Mat &ir8, cv::Mat &depth)
    {
        libfreenect2::FrameMap frames;
        if (!listener_.waitForNewFrame(frames, 2000)) return false;
        auto *ir = frames[libfreenect2::Frame::Ir];
        auto *d = frames[libfreenect2::Frame::Depth];
        cv::Mat irf(ir->height, ir->width, CV_32FC1, ir->data);
        cv::Mat df(d->height, d->width, CV_32FC1, d->data);

        // IR intensity is roughly 1/r^2; sqrt + fixed scale gives a stable, skin-friendly range
        cv::Mat s;
        cv::sqrt(cv::min(irf, 65535.f), s);
        s.convertTo(ir8, CV_8U, 255.0 / 140.0);
        cv::flip(ir8, ir8, 1);
        cv::flip(df, depth, 1);
        depth = depth.clone();
        listener_.release(frames);
        return true;
    }

    float fx_ = 365, fy_ = 365, cx_ = 256, cy_ = 212;

private:
    libfreenect2::Freenect2 fn_;
    libfreenect2::Freenect2Device *dev_ = nullptr;
    libfreenect2::SyncMultiFrameListener listener_{libfreenect2::Frame::Ir | libfreenect2::Frame::Depth};
};

// ---------------------------------------------------------------- face


class FaceEngine {
public:
    bool init()
    {
        try {
            det_ = cv::FaceDetectorYN::create(model_dir() + "/face_detection_yunet_2023mar.onnx", "",
                                              cv::Size(W, H), 0.7f, 0.3f, 5000);
            rec_ = cv::FaceRecognizerSF::create(model_dir() + "/face_recognition_sface_2021dec.onnx", "");
        } catch (const cv::Exception &e) {
            fprintf(stderr, "kface: cannot load models from %s: %s\n", model_dir().c_str(), e.what());
            return false;
        }
        return true;
    }

    // largest face in frame, analysed for liveness, with embedding
    bool analyse(const cv::Mat &ir8, const cv::Mat &depth, const Kinect &k, const Config &c, Face &f)
    {
        cv::Mat big, bgr, faces;
        cv::resize(ir8, big, cv::Size(W, H), 0, 0, cv::INTER_LINEAR);
        cv::cvtColor(big, bgr, cv::COLOR_GRAY2BGR);
        det_->detect(bgr, faces);
        if (faces.rows == 0) return false;

        int best = 0;
        for (int i = 1; i < faces.rows; i++)
            if (faces.at<float>(i, 2) * faces.at<float>(i, 3) > faces.at<float>(best, 2) * faces.at<float>(best, 3))
                best = i;
        f.det = faces.row(best).clone();

        cv::Mat aligned;
        rec_->alignCrop(bgr, f.det, aligned);
        rec_->feature(aligned, f.feature);
        f.feature = f.feature.clone();

        liveness(depth, {k.fx_, k.fy_, k.cx_, k.cy_}, c, f);
        return true;
    }

    float match(const cv::Mat &a, const cv::Mat &b)
    {
        return (float)rec_->match(a, b, cv::FaceRecognizerSF::FR_COSINE);
    }

    static constexpr int W = 1024, H = 848;  // 2x upscale of the 512x424 IR image

private:

    cv::Ptr<cv::FaceDetectorYN> det_;
    cv::Ptr<cv::FaceRecognizerSF> rec_;
};


// ---------------------------------------------------------------- commands

static void annotate(cv::Mat &img, const Face &f, float score)
{
    const float *d = f.det.ptr<float>();
    cv::Scalar col = f.live ? cv::Scalar(0, 255, 0) : cv::Scalar(0, 0, 255);
    cv::rectangle(img, cv::Rect(d[0] / 2, d[1] / 2, d[2] / 2, d[3] / 2), col, 1);
    for (int i = 0; i < 5; i++) cv::circle(img, {(int)(d[4 + 2 * i] / 2), (int)(d[5 + 2 * i] / 2)}, 2, col, -1);
    char buf[160];
    snprintf(buf, sizeof buf, "rel %.0f rms %.1f iod %.0f d %.0f s %.2f %s", f.relief, f.plane_rms, f.iod,
             f.dist, score, f.live ? "LIVE" : f.why.c_str());
    cv::putText(img, buf, {4, 14}, cv::FONT_HERSHEY_SIMPLEX, 0.4, col, 1);
}

static int cmd_debug(int n)
{
    Config c;
    load_config(c);
    Kinect k;
    FaceEngine fe;
    if (!fe.init() || !k.open()) return 2;
    fs::create_directories("debug");
    cv::Mat ir, depth, ref;
    for (int i = 0; i < n; i++) {
        if (!k.grab(ir, depth)) continue;
        cv::Mat out;
        cv::cvtColor(ir, out, cv::COLOR_GRAY2BGR);
        Face f;
        float s = 0;
        if (fe.analyse(ir, depth, k, c, f)) {
            if (ref.empty() && f.live) ref = f.feature;
            s = ref.empty() ? 0 : fe.match(ref, f.feature);
            annotate(out, f, s);
            printf("%3d relief %5.1f v %5.1f h %5.1f mm  plane_rms %4.1f mm  iod %4.0f mm  dist %4.0f mm  self-sim %.3f  %s\n", i,
                   f.relief, f.relief_v, f.relief_h, f.plane_rms, f.iod, f.dist, s, f.live ? "LIVE" : f.why.c_str());
        } else {
            printf("%3d no face\n", i);
        }
        cv::Mat dv;
        depth.convertTo(dv, CV_8U, 255.0 / 1500.0);
        cv::imwrite(cv::format("debug/ir_%03d.png", i), out);
        cv::imwrite(cv::format("debug/depth_%03d.png", i), dv);
    }
    return 0;
}

static int cmd_enroll(const std::string &user, int samples)
{
    Config c;
    load_config(c);
    Kinect k;
    FaceEngine fe;
    if (!fe.init() || !k.open()) return 2;
    auto faces = load_faces(user);
    printf("Look at the Kinect, slowly turn your head a little left/right/up/down...\n");
    cv::Mat ir, depth;
    int got = 0;
    auto t0 = Clock::now();
    while (got < samples && Clock::now() - t0 < std::chrono::seconds(60)) {
        if (!k.grab(ir, depth)) continue;
        Face f;
        if (!fe.analyse(ir, depth, k, c, f)) continue;
        if (!f.live) {
            printf("  rejected: %s\n", f.why.c_str());
            continue;
        }
        // skip near-duplicates to get pose diversity
        bool dup = false;
        for (size_t i = faces.size() - std::min<size_t>(faces.size(), got); i < faces.size(); i++)
            if (fe.match(faces[i], f.feature) > 0.92f) dup = true;
        if (dup) continue;
        faces.push_back(f.feature);
        printf("  sample %d/%d\n", ++got, samples);
    }
    if (got < samples / 2) {
        fprintf(stderr, "kface: too few usable samples (%d)\n", got);
        return 1;
    }
    if (!save_faces(user, faces)) {
        fprintf(stderr, "kface: cannot write %s\n", user_file(user).c_str());
        return 1;
    }
    printf("Enrolled %d samples for %s (%zu total)\n", got, user.c_str(), faces.size());
    return 0;
}

static int cmd_verify(const std::string &user)
{
    Config c;
    load_config(c);
    auto faces = load_faces(user);
    if (faces.empty()) return 3;  // not enrolled: let PAM fall through
    Kinect k;
    FaceEngine fe;
    if (!fe.init() || !k.open()) return 2;
    printf("Looking for your face...\n");
    fflush(stdout);
    cv::Mat ir, depth;
    int hits = 0;
    auto deadline = Clock::now() + std::chrono::duration<double>(c.timeout_s);
    while (Clock::now() < deadline) {
        if (!k.grab(ir, depth)) continue;
        Face f;
        if (!fe.analyse(ir, depth, k, c, f)) continue;
        float best = -1;
        for (auto &m : faces) best = std::max(best, fe.match(m, f.feature));
        if (getenv("KFACE_VERBOSE"))
            fprintf(stderr, "sim %.3f relief %.1f v %.1f h %.1f rms %.1f iod %.0f %s\n", best, f.relief, f.relief_v, f.relief_h, f.plane_rms, f.iod,
                    f.live ? "live" : f.why.c_str());
        if (f.live && best >= c.match_threshold) {
            if (++hits >= c.need_matches) return 0;
        }
    }
    return 1;
}

int main(int argc, char **argv)
{
    cv::utils::logging::setLogLevel(getenv("KFACE_VERBOSE") ? cv::utils::logging::LOG_LEVEL_INFO
                                                            : cv::utils::logging::LOG_LEVEL_SILENT);
    std::string cmd = argc > 1 ? argv[1] : "";
    // pam_exec passes the user in the environment
    if (cmd == "verify" && argc == 2 && getenv("PAM_USER")) {
        std::string user = getenv("PAM_USER");
        if (!valid_user(user)) return 4;
        // unprivileged callers (e.g. the lock screen) may only verify themselves
        if (geteuid() != 0) {
            struct passwd *pw = getpwuid(getuid());
            if (!pw || user != pw->pw_name) return 4;
        }
        return cmd_verify(user);
    }
    if (cmd == "debug") return cmd_debug(argc > 2 ? atoi(argv[2]) : 20);
    if ((cmd == "enroll" || cmd == "verify") && argc > 2) {
        std::string user = argv[2];
        if (!valid_user(user)) return 4;
        if (cmd == "enroll") {
            if (geteuid() != 0 && !getenv("KFACE_DATA")) {
                fprintf(stderr, "kface: enroll needs root (sudo kface enroll %s)\n", user.c_str());
                return 77;
            }
            return cmd_enroll(user, argc > 3 ? atoi(argv[3]) : 15);
        }
        return cmd_verify(user);
    }
    fprintf(stderr, "usage: kface debug [n] | enroll <user> [samples] | verify <user>\n");
    return 64;
}
