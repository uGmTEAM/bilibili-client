#!/bin/sh
# 冒烟测试：DASH 下载 + ffmpeg 合流（在 bookworm 容器内运行）
set -e
BIN=/app/dist/bilic-linux-x86_64
chmod +x "$BIN"
"$BIN" --version
rm -rf /tmp/smoke
mkdir -p /tmp/smoke
"$BIN" video download BV1U3n46GEzY -q 360p -o /tmp/smoke --overwrite 2>&1 | tail -6
find /tmp/smoke -type f -printf '%s\t%p\n'
find /tmp/smoke -name '*.mp4' | while read -r f; do
  echo "== $f"
  ffprobe -v error -show_entries format=duration,size:stream=codec_type,codec_name -of default=nw=1 "$f"
done
