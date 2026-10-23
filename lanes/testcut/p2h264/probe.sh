set -u
cd "$HOME/lanes/testcut-p2-h264/edith_codecs" 2>/dev/null || { echo "no lane tree"; exit 1; }
export PATH="$HOME/.cargo/bin:$PATH"
echo "host=$(hostname)"
command -v cargo-nextest ffmpeg python3 >/dev/null 2>&1 && echo TOOLS_OK
cargo nextest --version | head -1
cargo llvm-cov clean --help | grep -i profraw
cargo nextest run --help | grep -E -- "--filter-expr|-E," | head -2
cargo mutants -p ec-h264 --list 2>/dev/null | head -3
cargo mutants -p ec-h264 --list 2>/dev/null | wc -l
