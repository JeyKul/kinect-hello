#!/bin/bash
# Build everything as a normal user: libfreenect2 (pinned), face models, kface (+ GUI if Qt >= 6.5),
# kface-fido and kface-fprintd (if Rust is available). Usually run through ./install.sh.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
export PATH="$HOME/.cargo/bin:$PATH"
JOBS=$(nproc)

FREENECT2_COMMIT=fd64c5d9b214df6f6a55b4419357e51083f15d93
ZOO=https://github.com/opencv/opencv_zoo/raw
ZOO_MAIN=48b10fa505aeb51fbd127fc60a5c660040e823b7
ZOO_2022=c97242ce7f2a554e288b50eabd9f5df957e78801

if [ ! -f prefix/lib/libfreenect2.so ]; then
    [ -d libfreenect2 ] || git clone https://github.com/OpenKinect/libfreenect2.git
    git -C libfreenect2 checkout -q "$FREENECT2_COMMIT"
    # CPU depth decoding only: no OpenGL/OpenCL/CUDA/VAAPI dependencies
    cmake -S libfreenect2 -B libfreenect2/build -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$PWD/prefix" -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
        -DENABLE_CUDA=OFF -DENABLE_OPENCL=OFF -DENABLE_OPENGL=OFF -DENABLE_VAAPI=OFF \
        -DBUILD_EXAMPLES=OFF -DBUILD_OPENNI2_DRIVER=OFF \
        -DCMAKE_CXX_FLAGS="-include cstdint"
    cmake --build libfreenect2/build -j"$JOBS"
    cmake --install libfreenect2/build
fi

# face models, pinned to opencv_zoo commits and checked
mkdir -p models
fetch() { # file url sha256
    if [ ! -f "models/$1" ]; then
        curl -fsSL -o "models/$1.part" "$2"
        mv "models/$1.part" "models/$1"
    fi
    echo "$3  models/$1" | sha256sum -c --quiet
}
fetch face_detection_yunet_2023mar.onnx "$ZOO/$ZOO_MAIN/models/face_detection_yunet/face_detection_yunet_2023mar.onnx" \
    8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4
fetch face_detection_yunet_2022mar.onnx "$ZOO/$ZOO_2022/models/face_detection_yunet/face_detection_yunet_2022mar.onnx" \
    50ef07f702a31741ca46a4c0d947773b64143b9362780237bf0d427d6c79bab7
fetch face_recognition_sface_2021dec.onnx "$ZOO/$ZOO_MAIN/models/face_recognition_sface/face_recognition_sface_2021dec.onnx" \
    0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79

cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -Dfreenect2_DIR="$PWD/prefix/lib/cmake/freenect2"
cmake --build build -j"$JOBS"
./build/kface_tests 2>/dev/null
KFACE_MODELS="$PWD/models" ./build/kface selftest

if command -v cargo >/dev/null; then
    cargo build --release --manifest-path fprint/Cargo.toml
    if pkg-config --exists tss2-esys 2>/dev/null; then
        cargo build --release --manifest-path fido/Cargo.toml
        cargo test --release --manifest-path fido/Cargo.toml -q
    else
        echo "tss2-esys not found: skipping kface-fido (passkeys)"
    fi
else
    echo "cargo not found: skipping kface-fprintd and kface-fido"
fi
echo "build complete"
