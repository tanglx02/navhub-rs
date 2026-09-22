#!/usr/bin/env bash
# 把各平台可执行文件打包成 GitHub Releases 附件。
# 用法：bash scripts/package.sh [二进制目录]
#   输入：<二进制目录>/navhub-<平台>     （由 CI 的各构建任务产出，默认 packages）
#   输出：dist/navhub-<版本>-<平台>.tar.gz 与 dist/navhub-<版本>-SHA256SUMS.txt
# 压缩包内是一层 navhub/ 目录（可执行文件与 frontend/dist 同级），
# 解压到 /opt 即得 /opt/navhub，与 systemd 单元、README 的部署路径一致。
set -euo pipefail

cd "$(dirname "$0")/.."

BIN_DIR="${1:-packages}"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"

[ -d "$BIN_DIR" ] || { echo "未找到二进制目录：$BIN_DIR" >&2; exit 1; }
[ -d frontend/dist ] || { echo "缺少前端产物 frontend/dist" >&2; exit 1; }

# 递归查找：CI 的 artifact 下载可能带一层目录（packages/ 或 packages/out/）
mapfile -t bins < <(find "$BIN_DIR" -type f -name 'navhub-*' 2>/dev/null | sort)
[ "${#bins[@]}" -gt 0 ] || { echo "$BIN_DIR 下没有 navhub-* 可执行文件" >&2; exit 1; }

rm -rf .stage dist
mkdir -p dist

for src in "${bins[@]}"; do
  file="$(basename "$src")"
  plat="${file#navhub-}"
  bin="navhub"
  case "$plat" in
    windows-*) bin="navhub.exe" ;;
  esac

  stage=".stage/$plat/navhub"
  mkdir -p "$stage/frontend/dist" "$stage/scripts"
  cp "$src" "$stage/$bin"
  chmod +x "$stage/$bin"
  cp -r frontend/dist/. "$stage/frontend/dist/"
  cp .env.example "$stage/"
  cp scripts/navhub.service "$stage/scripts/"
  cp README.md LICENSE CHANGELOG.md "$stage/"

  out="dist/navhub-$VERSION-$plat.tar.gz"
  tar -czf "$out" -C ".stage/$plat" navhub
  echo "已生成 $out ($(du -h "$out" | cut -f1))"
done

rm -rf .stage
( cd dist && sha256sum navhub-*.tar.gz > "navhub-$VERSION-SHA256SUMS.txt" )

echo "---- 制品清单 ----"
cat dist/"navhub-$VERSION-SHA256SUMS.txt"
echo "---- 包结构示例 ----"
sample="$(ls dist/navhub-*-*.tar.gz | head -n 1)"
echo "$sample"
{ tar -tzf "$sample" | head -n 12; } || true
