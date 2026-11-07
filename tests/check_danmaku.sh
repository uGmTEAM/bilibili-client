#!/bin/sh
# 检查某视频弹幕里是否出现指定关键字
BIN=/app/dist/bilic-linux-x86_64
BV=$1
KEY=${2:-bilic}
rm -rf /tmp/dmcheck
"$BIN" video danmaku "$BV" -o /tmp/dmcheck >/dev/null 2>&1 || {
  echo "下载弹幕失败"
  exit 1
}
find /tmp/dmcheck -name '*.xml' | while read -r file; do
  total=$(grep -c '<d ' "$file")
  hits=$(grep -c "$KEY" "$file")
  echo "$file  弹幕数=$total  命中=$hits"
  grep -o "<d p=\"[^\"]*\">[^<]*$KEY[^<]*</d>" "$file" | head -5
done
