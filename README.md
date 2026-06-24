# IMCA Client v1.3

RoboMaster 自定义客户端 — 接收选手端 UDP H.265 图传，GPU 硬件加速解码，MJPEG 流显示。

> 本项目基于 [JNU-Shark](https://github.com/JNU-Shark) 的 SharkClient 开源项目优化改造而来，在此致谢。

## 快速启动

```bash
# 解压
tar -xzf IMCA_Client_v1.3.tar.gz

# 直接运行（资源文件在同目录下）
./IMCA_Client_v1_3
```

## 系统依赖

| 组件 | 要求 |
|------|------|
| 操作系统 | Ubuntu 22.04+ / Linux x86_64 |
| FFmpeg | 系统已安装（`sudo apt install ffmpeg`） |

### GPU 驱动（按显卡选装）

**Intel 核显：**
```bash
sudo apt install intel-media-va-driver vainfo
```

**AMD 核显：**
```bash
sudo apt install mesa-va-drivers vainfo
```

**NVIDIA 独显：**
```bash
sudo apt install nvidia-driver-535
```

确认 GPU 可用：
```bash
ffmpeg -hwaccels 2>/dev/null    # 应包含 vaapi/qsv/cuda
vainfo 2>&1 | head -10          # Intel/AMD 验证
nvidia-smi                      # NVIDIA 验证
```

## 网络配置

客户端通过网线直连选手端：

```
选手端 (192.168.12.1) ←——网线——→ 本机 (192.168.12.2)
```

设置本机 IP：
```bash
sudo ip addr add 192.168.12.2/24 dev enp67s0
sudo ip link set enp67s0 up
```

验证连通：
```bash
ping 192.168.12.1
```

## 使用说明

1. 启动客户端
2. 按 `P` 打开设置
3. 网络 tab：确认 UDP 端口 `3334`，MQTT 连接到 `192.168.12.1:3333`
4. 切换 tab 到"图传"，等待选手端推流

### 分辨率切换

在设置 → 网络 tab 的"输出分辨率"下拉框直接切换：

| 选项 | 说明 |
|------|------|
| 480p | 低带宽，适合网络差时 |
| 720p | 平衡 |
| 1080p | 默认，清晰 |
| 原始分辨率 | 不缩放 |

切换时画面会短暂中断约 1-2 秒，之后自动恢复。

## 环境变量（可选）

启动时可加环境变量调整参数，无需重新编译：

```bash
SHARK_JPEG_QUALITY=40 SHARK_MAX_FPS=60 ./IMCA_Client_v1_3
```

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `SHARK_JPEG_QUALITY` | 50 | JPEG 质量（1-100） |
| `SHARK_MAX_FPS` | 60 | 最大帧率 |
| `SHARK_DECODE_QUEUE` | 8 | 解码队列深度（1=最低延迟） |
| `SHARK_VAAPI_DEVICE` | `/dev/dri/renderD128` | VAAPI 渲染设备 |
| `SHARK_FFMPEG_PATH` | 自动探测 | 指定 FFmpeg 路径 |
| `SHARK_LOG` | `info` | 日志级别 |

## 推荐配置

```bash
# 竞赛模式（低延迟）
SHARK_DECODE_QUEUE=1 SHARK_JPEG_QUALITY=40 ./IMCA_Client_v1_3
```

## AI 检测（可选）

AI 检测是独立的 Python 服务，不启动也能正常使用视频流。

```bash
# 解压 AI 服务源码
unzip SharkVisionLiteServer-open-source-v1.0.0-20260519.zip
cd SharkVisionLiteServer
pip install onnxruntime opencv-python numpy
python pipe_server.py
```

客户端会自动通过 Unix Socket 连接 AI 服务。

## 从源码构建

### 安装所有依赖

```bash
# 1. Node.js 20+
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt install -y nodejs

# 2. Rust 工具链
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# 3. 系统构建依赖（Tauri + FFmpeg + turbojpeg）
sudo apt install -y \
  ffmpeg \
  cmake \
  pkg-config \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libjavascriptcoregtk-4.1-dev \
  libsoup-3.0-dev \
  libappindicator3-dev \
  librsvg2-dev \
  libssl-dev

# 4. GPU 驱动（按显卡选装）
# Intel 核显:
sudo apt install -y intel-media-va-driver vainfo
# AMD 核显:
sudo apt install -y mesa-va-drivers vainfo
# NVIDIA 独显:
sudo apt install -y nvidia-driver-535
```

### 编译

```bash
cd SharkClient-main
npm install
npm run tauri build
```

产物位置：
- 二进制：`src-tauri/target/release/IMCA_Client_v1_3`
- deb 包：`src-tauri/target/release/bundle/deb/`
- AppImage：`src-tauri/target/release/bundle/appimage/`

### 创建桌面快捷方式

```bash
cat > ~/.local/share/applications/IMCA_Client_v1_3.desktop << 'EOF'
[Desktop Entry]
Categories=
Comment=IMCA Client v1.3
Exec=<你的路径>/src-tauri/target/release/IMCA_Client_v1_3
StartupWMClass=IMCA_Client_v1_3
Icon=<你的路径>/src-tauri/target/release/bundle/appimage/IMCA_Client_v1_3.AppDir/usr/share/icons/hicolor/256x256@2/apps/IMCA_Client_v1_3.png
Name=IMCA Client v1.3
Terminal=false
Type=Application
EOF
```

> 把 `<你的路径>` 替换为实际的项目目录绝对路径。

## 常见问题

**黑屏 / 无画面：**
- 确认选手端在推流
- 确认网线已连接、IP 在同一网段
- 确认 UDP 端口 3334 没被防火墙挡住

**画面卡顿：**
- 降低分辨率（切换到 720p 或 480p）
- 增大队列：`SHARK_DECODE_QUEUE=4`

**MQTT 连接失败：**
- 检查选手端 IP 是否正确
- 杀掉旧进程：`pkill -9 -f IMCA_Client`
