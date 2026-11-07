#!/bin/sh
# 生成发行包（在 Linux / 容器内执行）
set -e
cd /app
NAME=bilic-0.1.0-linux-x86_64
OUT=dist/$NAME

rm -rf "$OUT" "dist/$NAME.tar.gz" dist/sha256sums.txt
mkdir -p "$OUT/completions"
install -m 755 dist/bilic-linux-x86_64 "$OUT/bilic"
cp README.md LICENSE THIRD-PARTY.md tests/install.sh "$OUT/"
chmod +x "$OUT/install.sh"

for shell in bash zsh fish powershell; do
	"$OUT/bilic" completions "$shell" > "$OUT/completions/bilic.$shell"
done
mv "$OUT/completions/bilic.zsh" "$OUT/completions/_bilic"

tar --owner=0 --group=0 -czf "dist/$NAME.tar.gz" -C dist "$NAME"
cp "$OUT/bilic" dist/bilic-linux-x86_64
cd dist
sha256sum "$NAME.tar.gz" bilic-linux-x86_64 > sha256sums.txt
ls -l "$NAME.tar.gz" bilic-linux-x86_64 sha256sums.txt
cat sha256sums.txt
