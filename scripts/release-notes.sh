#!/usr/bin/env bash
# 生成 GitHub Releases 的发布说明（release-notes.md），供 CI 使用。
# 用法：bash scripts/release-notes.sh <tag>
set -euo pipefail

TAG="${1:?缺少 tag（如 v1.0.0）}"
VER="${TAG#v}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

{
  echo "各平台预编译包。压缩包内的 \`navhub/\` 目录就是完整运行目录（可执行文件与 \`frontend/dist\` 同级），无需安装任何运行时。"
  echo
  echo "| 文件 | 适用平台 |"
  echo "| --- | --- |"
  echo "| \`navhub-$VER-linux-arm64.tar.gz\` | Linux aarch64：Armbian / 树莓派 4-5 / 香橙派（glibc ≥ 2.28） |"
  echo "| \`navhub-$VER-linux-amd64.tar.gz\` | Linux x86_64（glibc ≥ 2.28） |"
  echo "| \`navhub-$VER-windows-amd64.tar.gz\` | Windows 10/11 x64 |"
  echo "| \`navhub-$VER-macos-arm64.tar.gz\` | macOS Apple Silicon |"
  echo "| \`navhub-$VER-macos-amd64.tar.gz\` | macOS Intel |"
  echo
  echo "## 快速开始"
  echo
  echo '```bash'
  echo "# Linux / macOS（ARM64 设备示例）"
  echo "sudo tar -xzf navhub-$VER-linux-arm64.tar.gz -C /opt"
  echo "cd /opt/navhub && cp .env.example .env && ./navhub"
  echo "# 浏览器打开 http://<设备IP>:8100，默认账号 admin / admin123，登录后请立即修改密码"
  echo '```'
  echo
  echo '```powershell'
  echo "# Windows（tar 为 Win10+ 自带）"
  echo 'tar -xzf navhub-'"$VER"'-windows-amd64.tar.gz'
  echo 'cd navhub; .\navhub.exe'
  echo '```'
  echo
  echo "> 二进制未做代码签名。macOS 首次运行请执行 \`xattr -d com.apple.quarantine ./navhub\`；"
  echo "> Windows 若被 SmartScreen 拦截，选择「仍要运行」即可。"
  echo
  echo "## 更新内容"
  if [ -f "$ROOT/CHANGELOG.md" ]; then
    awk -v ver="$VER" '
      index($0, "## [v" ver "]") == 1 { grab = 1; next }
      grab && /^## / { exit }
      grab { print }
    ' "$ROOT/CHANGELOG.md"
  fi
} > release-notes.md

cat release-notes.md
