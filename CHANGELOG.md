# 更新日志

本项目遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [v1.0.0] - 2026-09-22

### 新增

- 使用 Rust + Axum 完整重写 NavHub 后端，功能与原 Python(FastAPI) 版 1:1 对齐：
  - 应用 / 分类管理（CRUD、排序、启停用、级联归类）
  - JWT 登录认证、修改密码、登录失败限速锁定
  - 图标上传（魔数校验 + 扩展名白名单 + 大小限制）
  - 站点设置、种子数据、SPA 静态托管与安全响应头
- 单文件静态二进制（release 约 3.5MB），无运行时依赖
- 18 项集成测试，与 Python 版测试契约逐项对齐
- systemd 服务单元与 `.env` 配置示例，适配 Armbian ARM64 资源受限环境
- GitHub Actions 多平台发布：推送 `v*` 标签自动构建 linux-arm64 / linux-amd64 / windows-amd64 / macos-arm64 / macos-amd64 预编译包并发布到 Releases（含 SHA256SUMS）

### 性能

- 空闲内存占用约 7MB（Python 版约 100MB+）
- 单二进制直接运行，冷启动毫秒级
