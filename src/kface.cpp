// kface - Kinect v2 IR + depth face authentication
//
//   kface debug  [n]        save n annotated IR frames to ./debug, print metrics
//   kface enroll <user>     capture and store IR face embeddings for <user>
//   kface verify <user>     exit 0 if a live, matching face is seen within timeout
//   kface import <file>     (via pkexec) install embeddings captured by kface-setup for the caller
//   kface remove            (via pkexec) delete the caller's enrolled faces
//
// Recognition runs on the IR image (works in darkness, screens are black in IR).
// Liveness uses the pixel-aligned depth map: the face must have real 3D relief
// (nose in front of eyes, non-planar surface) and a metric size of a human face.

#include "kface_sensor.hpp"
#include "kface_notify.hpp"

#include <opencv2/opencv.hpp>
#include <opencv2/core/utils/logger.hpp>

#include <fcntl.h>
#include <pwd.h>
#include <sys/stat.h>
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
namespace fs = std::filesystem;
using Clock = std::chrono::steady_clock;

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
    auto t0 = Clock::now();
    for (int i = 0; i < n && !stop_flag(); i++) {
        if (!k.grab(ir, depth)) continue;
        cv::Mat out;
        cv::cvtColor(ir, out, cv::COLOR_GRAY2BGR);
        Face f;
        float s = 0;
        if (fe.analyse(ir, depth, k, c, f)) {
            if (ref.empty() && f.live) ref = f.feature;
            s = ref.empty() ? 0 : fe.match(ref, f.feature);
            annotate(out, f, s);
            printf("%5.1fs yaw %5.1f (depth %5.1f) pitch %5.1f | ", std::chrono::duration<double>(Clock::now() - t0).count(),
                   f.yaw, f.yaw_depth, f.pitch);
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

// guided head pose recording, used to tune the attention check
static int cmd_pose()
{
    Config c;
    load_config(c);
    Kinect k;
    FaceEngine fe;
    if (!fe.init() || !k.open()) return 2;
    struct Phase {
        const char *say;
        double secs;
    } phases[] = {{"Look straight INTO THE KINECT", 5}, {"Look at your MAIN DISPLAY as usual", 5},
                  {"Look at the Kinect and NOD slowly twice", 6}, {"Look at the main display and NOD twice", 6}};
    cv::Mat ir, depth;
    for (auto &ph : phases) {
        printf("\n>>> %s\n", ph.say);
        fflush(stdout);
        std::vector<float> yaw, pitch;
        auto t0 = Clock::now();
        double t;
        while ((t = std::chrono::duration<double>(Clock::now() - t0).count()) < ph.secs && !stop_flag()) {
            if (!k.grab(ir, depth)) continue;
            Face f;
            if (!fe.analyse(ir, depth, k, c, f) || f.why == "no depth" || f.why == "sparse depth" || t < 1.0) continue;
            yaw.push_back(f.yaw);
            pitch.push_back(f.pitch);
            printf("    %4.1fs yaw %6.1f  pitch %6.1f  %s\n", t, f.yaw, f.pitch, f.live ? "live" : f.why.c_str());
        }
        auto stat = [](std::vector<float> v, const char *n) {
            if (v.empty()) return;
            std::sort(v.begin(), v.end());
            printf("  %s median %6.1f  range %6.1f .. %6.1f\n", n, v[v.size() / 2], v.front(), v.back());
        };
        stat(yaw, "yaw  ");
        stat(pitch, "pitch");
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
    while (got < samples && Clock::now() - t0 < std::chrono::seconds(60) && !stop_flag()) {
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
    struct passwd *pw = getpwnam(user.c_str());
    uid_t uid = pw ? pw->pw_uid : (uid_t)-1;
    // the caller (e.g. kface-fido) may say what the scan is for
    const char *reason = getenv("KFACE_REASON");
    if (pw) notify_overlay(uid, "scan", reason ? reason : "");
    printf("Looking for your face...\n");
    fflush(stdout);
    cv::Mat ir, depth;
    Streak streak;
    auto t0 = Clock::now();
    auto deadline = t0 + std::chrono::duration<double>(c.timeout_s);
    while (Clock::now() < deadline && !stop_flag()) {
        if (!k.grab(ir, depth)) continue;
        Face f;
        double t = std::chrono::duration<double>(Clock::now() - t0).count();
        if (!fe.analyse(ir, depth, k, c, f)) {
            streak.update(t, false, c.attention_s);
            continue;
        }
        float best = -1;
        for (auto &m : faces) best = std::max(best, fe.match(m, f.feature));
        if (getenv("KFACE_VERBOSE"))
            fprintf(stderr, "%.2fs sim %.3f yaw %.1f relief %.1f v %.1f h %.1f rms %.1f iod %.0f %s\n", t, best, f.yaw,
                    f.relief, f.relief_v, f.relief_h, f.plane_rms, f.iod, f.live ? "live" : f.why.c_str());
        if (streak.update(t, f.live && best >= c.match_threshold && facing_sensor(f, c), c.attention_s)) {
            if (pw) notify_overlay(uid, "ok");
            return 0;
        }
    }
    if (pw) notify_overlay(uid, "fail");
    return 1;
}

// The user who invoked us through pkexec or sudo; import/remove only ever act on that user.
static bool calling_user(std::string &user, uid_t &uid)
{
    const char *e = getenv("PKEXEC_UID");
    if (!e) e = getenv("SUDO_UID");
    if (geteuid() != 0 || !e) return false;
    struct passwd *pw = getpwuid((uid_t)strtoul(e, nullptr, 10));
    if (!pw || !valid_user(pw->pw_name)) return false;
    user = pw->pw_name;
    uid = pw->pw_uid;
    return true;
}

static int cmd_import(const char *path)
{
    std::string user;
    uid_t uid;
    if (!calling_user(user, uid)) {
        fprintf(stderr, "kface: run as: pkexec kface import <file>\n");
        return 77;
    }
    // read only a regular file that the caller owns (we run as root)
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);
    struct stat st;
    if (fd < 0 || fstat(fd, &st) != 0 || !S_ISREG(st.st_mode) || st.st_uid != uid || st.st_size > (1 << 20)) {
        fprintf(stderr, "kface: refusing to read %s\n", path);
        if (fd >= 0) close(fd);
        return 1;
    }
    std::string buf(st.st_size, '\0');
    bool ok = read(fd, buf.data(), buf.size()) == (ssize_t)buf.size();
    close(fd);
    std::vector<cv::Mat> faces;
    if (ok) {
        try {
            cv::FileStorage fsr(buf, cv::FileStorage::READ | cv::FileStorage::MEMORY);
            for (auto n : fsr["faces"]) {
                cv::Mat m;
                n >> m;
                faces.push_back(m);
            }
        } catch (const cv::Exception &) {
            faces.clear();
        }
    }
    if (!valid_faces(faces)) {
        fprintf(stderr, "kface: %s is not a valid face file\n", path);
        return 1;
    }
    if (!save_faces(user, faces)) {
        fprintf(stderr, "kface: cannot write %s\n", user_file(user).c_str());
        return 1;
    }
    printf("Installed %zu samples for %s\n", faces.size(), user.c_str());
    return 0;
}

static int cmd_remove()
{
    std::string user;
    uid_t uid;
    if (!calling_user(user, uid)) {
        fprintf(stderr, "kface: run as: pkexec kface remove\n");
        return 77;
    }
    std::error_code ec;
    fs::remove(user_file(user), ec);
    printf("Removed faces for %s\n", user.c_str());
    return ec ? 1 : 0;
}

int main(int argc, char **argv)
{
    cv::utils::logging::setLogLevel(getenv("KFACE_VERBOSE") ? cv::utils::logging::LOG_LEVEL_INFO
                                                            : cv::utils::logging::LOG_LEVEL_SILENT);
    setvbuf(stdout, nullptr, _IOLBF, 0);
    install_stop_handler();
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
    if (cmd == "import" && argc == 3) return cmd_import(argv[2]);
    if (cmd == "remove" && argc == 2) return cmd_remove();
    if (cmd == "pose") return cmd_pose();
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
    fprintf(stderr, "usage: kface debug [n] | enroll <user> [samples] | verify <user> | import <file> | remove\n");
    return 64;
}
