// kface core: config, depth liveness and face storage (no sensor dependencies, unit-testable)
#pragma once

#include <opencv2/core.hpp>
#include <opencv2/core/persistence.hpp>

#include <grp.h>
#include <pwd.h>
#include <unistd.h>

#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

namespace kface {

namespace fs = std::filesystem;

inline std::string model_dir()
{
    const char *e = getenv("KFACE_MODELS");
    return e ? e : "/usr/local/share/kface";
}

inline std::string data_dir()
{
    const char *e = getenv("KFACE_DATA");
    return e ? e : "/etc/kface";
}

struct Config {
    float match_threshold = 0.45f;  // SFace cosine similarity
    float min_relief_mm = 8.f;      // nose ahead of eye plane
    float max_relief_mm = 60.f;
    float min_relief_v_mm = 8.f;    // nose ahead of nasal bridge (defeats paper bent left/right)
    float min_relief_h_mm = 8.f;    // nose ahead of cheeks (defeats paper bent up/down)
    float min_plane_rms_mm = 3.f;   // a flat photo is ~1 mm
    float min_iod_mm = 48.f;        // interocular distance of a real adult face
    float max_iod_mm = 80.f;
    float min_dist_mm = 300.f;
    float max_dist_mm = 1300.f;
    double timeout_s = 4.0;
    int need_matches = 2;           // live matching frames required
};

inline void load_config(Config &c)
{
    std::ifstream f(data_dir() + "/kface.conf");
    std::string k;
    double v;
    while (f >> k >> v) {
        if (k == "match_threshold") c.match_threshold = v;
        else if (k == "min_relief_mm") c.min_relief_mm = v;
        else if (k == "min_relief_v_mm") c.min_relief_v_mm = v;
        else if (k == "min_relief_h_mm") c.min_relief_h_mm = v;
        else if (k == "min_plane_rms_mm") c.min_plane_rms_mm = v;
        else if (k == "timeout_s") c.timeout_s = v;
        else if (k == "need_matches") c.need_matches = v;
    }
}

struct Intrinsics {
    float fx, fy, cx, cy;
};

struct Face {
    cv::Mat det;          // YuNet row: box(4) landmarks(10) score
    cv::Mat feature;
    float relief = 0, relief_v = 0, relief_h = 0, plane_rms = 0, iod = 0, dist = 0;
    bool live = false;
    std::string why;
};

// Landmarks in Face::det are in 2x upscaled IR coordinates; depth is the 512x424 map in mm.
// median depth in a small window around a point given in upscaled coordinates
inline float depth_at(const cv::Mat &depth, float ux, float uy, int r = 3)
{
    int x = (int)(ux / 2), y = (int)(uy / 2);
    std::vector<float> v;
    for (int j = y - r; j <= y + r; j++)
        for (int i = x - r; i <= x + r; i++)
            if (i >= 0 && j >= 0 && i < depth.cols && j < depth.rows) {
                float d = depth.at<float>(j, i);
                if (d > 0) v.push_back(d);
            }
    if (v.size() < 5) return 0;
    std::nth_element(v.begin(), v.begin() + v.size() / 2, v.end());
    return v[v.size() / 2];
}

inline void liveness(const cv::Mat &depth, const Intrinsics &k, const Config &c, Face &f)
{
    const float *d = f.det.ptr<float>();
    // landmarks: right eye, left eye, nose, right mouth, left mouth
    float re = depth_at(depth, d[4], d[5]), le = depth_at(depth, d[6], d[7]);
    float no = depth_at(depth, d[8], d[9], 2);
    float rm = depth_at(depth, d[10], d[11]), lm = depth_at(depth, d[12], d[13]);
    if (!re || !le || !no || !rm || !lm) {
        f.why = "no depth";
        return;
    }
    // nose relative to the plane through eyes and mouth corners
    f.relief = (re + le + rm + lm) / 4 - no;
    f.dist = no;

    // Paper bends in one direction only; a face curves away from the nose both ways.
    // Vertical: the nasal bridge between the eyes lies on the nose's column (the mouth does too,
    // but lips sit about level with the nose tip). Horizontal: cheeks beside the nose, half an
    // eye distance out along the eye axis.
    float mc = depth_at(depth, (d[4] + d[6]) / 2, (d[5] + d[7]) / 2);
    float ex = (d[6] - d[4]) / 2, ey = (d[7] - d[5]) / 2;
    float cr = depth_at(depth, d[8] - ex, d[9] - ey), cl = depth_at(depth, d[8] + ex, d[9] + ey);
    if (!mc || !cr || !cl) {
        f.why = "no depth";
        return;
    }
    f.relief_v = mc - no;
    f.relief_h = (cr + cl) / 2 - no;

    // metric interocular distance
    float px = std::hypot(d[4] - d[6], d[5] - d[7]) / 2;
    f.iod = px * ((re + le) / 2) / k.fx;

    // planarity of the inner face region: least-squares plane fit residual
    int x0 = d[0] / 2 + d[2] / 8, x1 = (d[0] + d[2]) / 2 - d[2] / 8;
    int y0 = d[1] / 2 + d[3] / 8, y1 = (d[1] + d[3]) / 2 - d[3] / 8;
    std::vector<cv::Vec3f> pts;
    for (int y = std::max(0, y0); y < std::min(depth.rows, y1); y++)
        for (int x = std::max(0, x0); x < std::min(depth.cols, x1); x++) {
            float z = depth.at<float>(y, x);
            if (z > 0 && std::fabs(z - no) < 150)
                pts.push_back({(x - k.cx) * z / k.fx, (y - k.cy) * z / k.fy, z});
        }
    if (pts.size() < 50) {
        f.why = "sparse depth";
        return;
    }
    cv::Mat A((int)pts.size(), 3, CV_32F), b((int)pts.size(), 1, CV_32F), sol;
    for (size_t i = 0; i < pts.size(); i++) {
        A.at<float>(i, 0) = pts[i][0];
        A.at<float>(i, 1) = pts[i][1];
        A.at<float>(i, 2) = 1;
        b.at<float>(i) = pts[i][2];
    }
    cv::solve(A, b, sol, cv::DECOMP_SVD);
    cv::Mat res = A * sol - b;
    f.plane_rms = (float)std::sqrt(res.dot(res) / pts.size());

    if (f.dist < c.min_dist_mm || f.dist > c.max_dist_mm) f.why = "distance";
    else if (f.iod < c.min_iod_mm || f.iod > c.max_iod_mm) f.why = "face size";
    else if (f.relief < c.min_relief_mm || f.relief > c.max_relief_mm) f.why = "flat (relief)";
    else if (f.relief_v < c.min_relief_v_mm) f.why = "bent (vertical)";
    else if (f.relief_h < c.min_relief_h_mm) f.why = "bent (horizontal)";
    else if (f.plane_rms < c.min_plane_rms_mm) f.why = "flat (plane)";
    else f.live = true;
}

// ---------------------------------------------------------------- storage

inline fs::path user_file(const std::string &user) { return fs::path(data_dir()) / (user + ".faces"); }

inline std::vector<cv::Mat> load_faces(const std::string &user)
{
    std::vector<cv::Mat> out;
    cv::FileStorage fsr(user_file(user).string(), cv::FileStorage::READ);
    if (!fsr.isOpened()) return out;
    for (auto n : fsr["faces"]) {
        cv::Mat m;
        n >> m;
        out.push_back(m);
    }
    return out;
}

inline bool save_faces(const std::string &user, const std::vector<cv::Mat> &faces)
{
    fs::create_directories(data_dir());
    auto tmp = user_file(user).string() + ".tmp";
    {
        cv::FileStorage fsw(tmp, cv::FileStorage::WRITE);
        if (!fsw.isOpened()) return false;
        fsw << "faces" << "[";
        for (auto &m : faces) fsw << m;
        fsw << "]";
    }
    fs::permissions(tmp, fs::perms::owner_read | fs::perms::owner_write);
    if (geteuid() == 0) {
        if (struct passwd *pw = getpwnam(user.c_str())) {
            if (chown(tmp.c_str(), 0, pw->pw_gid) == 0)
                fs::permissions(tmp, fs::perms::owner_read | fs::perms::owner_write | fs::perms::group_read);
        }
    }
    fs::rename(tmp, user_file(user));
    return true;
}

inline bool valid_user(const std::string &u)
{
    if (u.empty() || u.size() > 64) return false;
    for (char ch : u)
        if (!isalnum((unsigned char)ch) && ch != '_' && ch != '-' && ch != '.') return false;
    return u[0] != '.';
}

}  // namespace kface
