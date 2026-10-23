#!/usr/bin/env bash
export EC_REQUIRE_FIXTURES=1
cargo clippy --release -p ec-h265 --tests -- -D warnings 2>&1 | tail -15; echo "clippy rc=${PIPESTATUS[0]}"
cargo test --release -p ec-h265 2>&1 | grep -E "^test result|FAILED|failed|panicked|^error" | head -30; echo "test rc=${PIPESTATUS[0]}"
echo GATE_DONE
