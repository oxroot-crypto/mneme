#!/usr/bin/env bash
# fuzz 长跑脚本(设计 14 §5):每个目标默认 1 小时;发布前累计 24 小时可分多轮。
#
# 用法:
#   DURATION=3600 ./fuzz/scripts/run_long.sh          # 每目标 1h(夜跑)
#   DURATION=86400 TARGETS="fuzz_hidx" ./fuzz/scripts/run_long.sh
#
# 需要 nightly 工具链 + cargo-fuzz:`cargo install cargo-fuzz --locked`。
set -euo pipefail

cd "$(dirname "$0")/.."

DURATION="${DURATION:-3600}"
TARGETS="${TARGETS:-fuzz_vsec fuzz_msec fuzz_hidx fuzz_wal_replay fuzz_dsl}"
CORPUS_DIR="${CORPUS_DIR:-corpus}"

for target in $TARGETS; do
  echo "==> fuzz ${target} (max_total_time=${DURATION}s)"
  # shellcheck disable=SC2086
  cargo +nightly fuzz run "${target}" "${CORPUS_DIR}/${target}" -- -max_total_time="${DURATION}"
done

echo "全部目标完成;每目标 ≥ ${DURATION}s(发布前累计 ≥ 24h)"
