// kface sensor + face engine: Kinect v2 capture, YuNet detection, SFace embeddings
#pragma once

#include "kface_core.hpp"

#include <libfreenect2/libfreenect2.hpp>
#include <libfreenect2/frame_listener_impl.h>
#include <libfreenect2/packet_pipeline.h>
#include <libfreenect2/logger.h>
#include <opencv2/imgproc.hpp>
#include <opencv2/objdetect/face.hpp>

#include <cstdio>

namespace kface {

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

}  // namespace kface
