#!/bin/sh
# 登录态只读验证（不发送、不投稿）
BIN=/app/dist/bilic-linux-x86_64
export HOME=/data

echo "=== whoami"
"$BIN" whoami

echo "=== vip"
"$BIN" vip

echo "=== message unread"
"$BIN" message unread

echo "=== message list (前 3 条会话)"
"$BIN" --json message list 2>&1 | head -c 1200; echo

echo "=== 与自己的历史消息（若支持）"
"$BIN" --json message history 1143250330 --count 3 2>&1 | head -c 800; echo

echo "=== season list（自己的合集）"
"$BIN" --json season list --size 5 2>&1 | head -c 600; echo
