#!/bin/bash
# Build everything as a normal user: libfreenect2 (pinned), face models, kface + GUI, kface-fido.
# Then install with: sudo ./install.sh
set -euo pipefail
cd "$(dirname "$0")"

FREENECT2_COMMIT=fd64c5d9b214df6f6a55b4419357e51083f15d93
ZOO=https://github.com/opencv/opencv_zoo/raw/main/models

if [ ! -f prefix/lib/libfreenect2.so ]; then
    [ -d libfreenect2 ] || git clone https://github.com/OpenKinect/libfreenect2.git
    git -C libfreenect2 checkout -q "$FREENECT2_COMMIT"
    cmake -S libfreenect2 -B libfreenect2/build -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX="$PWD/prefix" -DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
        -DENABLE_CUDA=OFF -DENABLE_OPENCL=OFF -DBUILD_OPENNI2_DRIVER=OFF \
        -DCMAKE_CXX_FLAGS="-include cstdint"
    cmake --build libfreenect2/build -j"$(nproc)"
    cmake --install libfreenect2/build
fi

mkdir -p models
fetch() { # file sha256
    [ -f "models/$1" ] || curl -fL -o "models/$1" "$2/$1"
    echo "$3  models/$1" | sha256sum -c --quiet
}
fetch face_detection_yunet_2023mar.onnx "$ZOO/face_detection_yunet" 8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4
fetch face_recognition_sface_2021dec.onnx "$ZOO/face_recognition_sface" 0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79

cmake -S . -B build -Dfreenect2_DIR="$PWD/prefix/lib/cmake/freenect2"
cmake --build build -j"$(nproc)"
./build/kface_tests

if command -v cargo >/dev/null && [ -e /dev/tpmrm0 ]; then
    cargo build --release --manifest-path fido/Cargo.toml
    cargo test --release --manifest-path fido/Cargo.toml
fi
if command -v cargo >/dev/null; then
    cargo build --release --manifest-path fprint/Cargo.toml
fi
echo "built. install with: sudo ./install.sh"
