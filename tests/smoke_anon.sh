#!/bin/sh
# 冒烟测试：匿名单功能（在 bookworm 容器内运行）
BIN=/app/dist/bilic-linux-x86_64
BV=BV1U3n46GEzY
rm -rf /tmp/anonsmoke
mkdir -p /tmp/anonsmoke

echo "--- video info"
"$BIN" --json video info "$BV" | head -c 200; echo

echo "--- comment list"
"$BIN" --json comment list "$BV" --size 2 | head -c 300; echo

echo "--- cover"
"$BIN" video cover "$BV" -o /tmp/anonsmoke/cover.jpg && ls -l /tmp/anonsmoke/cover.jpg

echo "--- danmaku xml + ass"
"$BIN" video danmaku "$BV" -o /tmp/anonsmoke/dm.xml && ls -l /tmp/anonsmoke/dm.xml
"$BIN" video danmaku "$BV" --ass -o /tmp/anonsmoke/dm.ass && ls -l /tmp/anonsmoke/dm.ass

echo "--- danmaku send dry-run"
"$BIN" danmaku send "$BV" "smoke test" --at 10s --dry-run

echo "--- completions"
"$BIN" completions bash | head -3
