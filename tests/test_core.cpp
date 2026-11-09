// Sensor-free tests for kface_core: synthetic depth maps for liveness, plus storage and input checks.

#include "../src/kface_core.hpp"

#include <cstdio>
#include <functional>

using namespace kface;

static int failures = 0;

#define CHECK(cond)                                                       \
    do {                                                                  \
        if (!(cond)) {                                                    \
            fprintf(stderr, "  FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            failures++;                                                   \
        }                                                                 \
    } while (0)

// ---------------------------------------------------------------- synthetic scenes

static const Intrinsics K{365, 365, 256, 212};
static const float NX = 256, NY = 212;  // nose tip pixel in the 512x424 IR image

// Depth map as a function of pixel offset from the nose tip; background at 2.5 m.
static cv::Mat scene(const std::function<float(float, float)> &z)
{
    cv::Mat d(424, 512, CV_32F, cv::Scalar(2500));
    for (int y = 0; y < d.rows; y++)
        for (int x = 0; x < d.cols; x++)
            if (std::fabs(x - NX) < 45 && std::fabs(y - NY) < 50) d.at<float>(y, x) = z(x - NX, y - NY);
    return d;
}

// YuNet-style detection row in 2x upscaled coordinates for a frontal face of the given
// metric interocular distance at the given distance.
// nose_dx shifts the nose landmark sideways (IR pixels), as it appears when the head turns.
static cv::Mat detection(float iod_mm, float dist_mm, float nose_dx = 0)
{
    float e = iod_mm * K.fx / dist_mm / 2;  // half eye distance in IR pixels
    float pts[10] = {NX - e, NY - 0.7f * e, NX + e, NY - 0.7f * e, NX + nose_dx, NY + 0.25f * e,
                     NX - 0.7f * e, NY + 1.2f * e, NX + 0.7f * e, NY + 1.2f * e};
    cv::Mat det(1, 15, CV_32F);
    float *p = det.ptr<float>();
    p[0] = 2 * (NX - 1.8f * e);
    p[1] = 2 * (NY - 2.0f * e);
    p[2] = 2 * 3.6f * e;
    p[3] = 2 * 4.0f * e;
    for (int i = 0; i < 10; i++) p[4 + i] = 2 * pts[i];
    p[14] = 0.95f;
    return det;
}

static Face run(const cv::Mat &depth, float iod_mm = 64, float dist_mm = 700, float nose_dx = 0)
{
    Config c;
    Face f;
    f.det = detection(iod_mm, dist_mm, nose_dx);
    liveness(depth, K, c, f);
    return f;
}

// Face-like surface: curves away from the nose in every direction, nose protrudes ~20 mm.
static float face_z(float dx, float dy, float z0 = 700)
{
    float r2 = dx * dx + dy * dy;
    return z0 + 0.06f * r2 - 20 * std::exp(-r2 / 50);
}

// ---------------------------------------------------------------- liveness

static void test_real_face()
{
    Face f = run(scene([](float dx, float dy) { return face_z(dx, dy); }));
    CHECK(f.live);
    CHECK(f.relief > 15);
    CHECK(f.relief_v > 8 && f.relief_h > 8);
    CHECK(std::fabs(f.iod - 64) < 4);
    CHECK(std::fabs(f.dist - 680) < 15);
}

static void test_flat_photo()
{
    Face f = run(scene([](float, float) { return 700.f; }));
    CHECK(!f.live);
    CHECK(f.relief < 1);
}

static void test_tilted_photo()
{
    // paper tilted ~60 degrees towards the camera at the bottom
    Face f = run(scene([](float, float dy) { return 700 - 1.7f * dy; }));
    CHECK(!f.live);
}

static void test_photo_bent_left_right()
{
    // printed photo curved around a vertical axis: nose ahead of eyes and mouth, but not of the bridge
    Face f = run(scene([](float dx, float) { return 700 + 0.1f * dx * dx; }));
    CHECK(!f.live);
    CHECK(f.relief > 8);  // would have fooled the plain relief check
    CHECK(f.why == "bent (vertical)");
}

static void test_photo_bent_up_down()
{
    Face f = run(scene([](float, float dy) { return 700 + 0.1f * dy * dy; }));
    CHECK(!f.live);
    CHECK(f.relief > 8);
    CHECK(f.why == "bent (horizontal)");
}

static void test_small_photo()
{
    // a 3D-shaped but half-size face (photo of a face, or a child's doll) at 70 cm
    Face f = run(scene([](float dx, float dy) { return face_z(dx * 2, dy * 2); }), 32);
    CHECK(!f.live);
    CHECK(f.why == "face size");
}

static void test_too_far()
{
    Face f = run(scene([](float dx, float dy) { return face_z(dx * 3, dy * 3, 2000); }), 64, 2000);
    CHECK(!f.live);
    CHECK(f.why == "distance");
}

static void test_no_depth()
{
    // glossy phone screens return no depth
    Face f = run(scene([](float, float) { return 0.f; }));
    CHECK(!f.live);
    CHECK(f.why == "no depth");
}

static void test_depth_at_ignores_holes()
{
    cv::Mat d(424, 512, CV_32F, cv::Scalar(0));
    d(cv::Rect(100, 100, 3, 3)).setTo(800);
    CHECK(depth_at(d, 2 * 101, 2 * 101, 1) == 800);
    CHECK(depth_at(d, 2 * 300, 2 * 300) == 0);
    CHECK(depth_at(d, -50, -50) == 0);
}

// ---------------------------------------------------------------- attention

static void test_facing_sensor()
{
    Config c;
    auto depth = scene([](float dx, float dy) { return face_z(dx, dy); });
    Face front = run(depth);
    CHECK(std::fabs(front.yaw) < 5);
    CHECK(facing_sensor(front, c));

    // nose displaced by relief * sin(40 deg): head turned towards another screen
    float px = front.relief * std::sin(40 * CV_PI / 180) / (front.dist / K.fx);
    Face turned = run(depth, 64, 700, px);
    CHECK(turned.yaw > 30);
    CHECK(!facing_sensor(turned, c));
    Face other = run(depth, 64, 700, -px);
    CHECK(other.yaw < -30);
    CHECK(!facing_sensor(other, c));
}

static void test_streak()
{
    Streak s;
    CHECK(!s.update(0.0, true, 0.6));
    CHECK(!s.update(0.5, true, 0.6));
    CHECK(s.update(0.6, true, 0.6));
    // any interruption (glance away, lost face) restarts the clock
    CHECK(!s.update(0.7, false, 0.6));
    CHECK(!s.update(0.8, true, 0.6));
    CHECK(!s.update(1.3, true, 0.6));
    CHECK(s.update(1.45, true, 0.6));
}

// ---------------------------------------------------------------- storage / input

static void test_valid_user()
{
    CHECK(valid_user("jey"));
    CHECK(valid_user("first.last-2_x"));
    CHECK(!valid_user(""));
    CHECK(!valid_user(".hidden"));
    CHECK(!valid_user("../etc/passwd"));
    CHECK(!valid_user("a/b"));
    CHECK(!valid_user("a b"));
    CHECK(!valid_user(std::string(65, 'a')));
}

static void test_storage_roundtrip()
{
    auto dir = fs::temp_directory_path() / ("kface-test-" + std::to_string(getpid()));
    setenv("KFACE_DATA", dir.c_str(), 1);

    CHECK(load_faces("nobody").empty());

    std::vector<cv::Mat> in;
    for (int i = 0; i < 3; i++) {
        cv::Mat m(1, 128, CV_32F);
        cv::randu(m, -1, 1);
        in.push_back(m);
    }
    CHECK(save_faces("alice", in));
    auto out = load_faces("alice");
    CHECK(out.size() == in.size());
    for (size_t i = 0; i < std::min(in.size(), out.size()); i++) CHECK(cv::norm(in[i], out[i]) < 1e-5);

    // biometric data must never be world readable
    auto perms = fs::status(user_file("alice")).permissions();
    CHECK((perms & fs::perms::others_all) == fs::perms::none);
    CHECK(!fs::exists(user_file("alice").string() + ".tmp"));

    std::ofstream(dir / "kface.conf") << "match_threshold 0.6\ntimeout_s 2.5\n";
    Config c;
    load_config(c);
    CHECK(std::fabs(c.match_threshold - 0.6f) < 1e-6);
    CHECK(c.timeout_s == 2.5);
    CHECK(c.min_relief_mm == Config{}.min_relief_mm);

    fs::remove_all(dir);
    unsetenv("KFACE_DATA");
}

int main()
{
    struct {
        const char *name;
        void (*fn)();
    } tests[] = {
        {"real face is live", test_real_face},
        {"flat photo rejected", test_flat_photo},
        {"tilted photo rejected", test_tilted_photo},
        {"photo bent left/right rejected", test_photo_bent_left_right},
        {"photo bent up/down rejected", test_photo_bent_up_down},
        {"undersized face rejected", test_small_photo},
        {"distant face rejected", test_too_far},
        {"missing depth rejected", test_no_depth},
        {"depth_at skips holes", test_depth_at_ignores_holes},
        {"head turned away is not attentive", test_facing_sensor},
        {"attention streak timing", test_streak},
        {"username validation", test_valid_user},
        {"face storage and config", test_storage_roundtrip},
    };
    for (auto &t : tests) {
        int before = failures;
        t.fn();
        printf("%s  %s\n", failures == before ? "ok  " : "FAIL", t.name);
    }
    printf("%d failure(s)\n", failures);
    return failures ? 1 : 0;
}
