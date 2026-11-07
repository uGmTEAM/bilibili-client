#!/bin/sh
# bilic 安装脚本：PREFIX=/usr/local sh install.sh
set -e
PREFIX=${PREFIX:-/usr/local}
DIR=$(cd "$(dirname "$0")" && pwd)

install -d "$PREFIX/bin"
install -m 755 "$DIR/bilic" "$PREFIX/bin/bilic"

if [ -d "$PREFIX/share/bash-completion/completions" ] && [ -f "$DIR/completions/bilic.bash" ]; then
	install -m 644 "$DIR/completions/bilic.bash" "$PREFIX/share/bash-completion/completions/bilic"
fi
if [ -d "$PREFIX/share/zsh/site-functions" ] && [ -f "$DIR/completions/_bilic" ]; then
	install -m 644 "$DIR/completions/_bilic" "$PREFIX/share/zsh/site-functions/_bilic"
fi
if [ -d "$PREFIX/share/fish/vendor_completions.d" ] && [ -f "$DIR/completions/bilic.fish" ]; then
	install -m 644 "$DIR/completions/bilic.fish" "$PREFIX/share/fish/vendor_completions.d/bilic.fish"
fi

echo "已安装: $PREFIX/bin/bilic"
"$PREFIX/bin/bilic" --version || true
