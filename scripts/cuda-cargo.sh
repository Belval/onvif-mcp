#!/bin/sh
set -eu
cuda_root=${CUDA_PATH:-$HOME/.local/share/onvif-cuda-12.2}
export CUDA_PATH="$cuda_root"
export CUDAToolkit_ROOT="$cuda_root"
export CUDACXX="$cuda_root/bin/nvcc"
export PATH="$cuda_root/bin:$PATH"
export LD_LIBRARY_PATH="$cuda_root/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# Native device code for this machine's GTX 1080 Ti (Pascal).
export CMAKE_CUDA_ARCHITECTURES=${CMAKE_CUDA_ARCHITECTURES:-61-real}
if [ "$#" -eq 0 ]; then set -- build --release --locked; fi
cargo_command=$1
shift
exec cargo "$cargo_command" --features cuda "$@"
