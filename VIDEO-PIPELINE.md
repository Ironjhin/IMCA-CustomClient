# IMCA Client v1.3 视频管线使用指南

> 本文档面向 RoboMaster 比赛场景，说明如何配置 IMCA Client 接收选手端 UDP H.265 图传、通过 GPU 硬件加速解码、以 MJPEG 流显示在前端。支持 Intel / AMD / NVIDIA 三种显卡。

---

## 目录

- [系统要求](#系统要求)
- [网络配置](#网络配置)
- [快速启动](#快速启动)
- [环境变量调参](#环境变量调参)
- [架构概览](#架构概览)
- [延迟分析](#延迟分析)
- [常见问题排查](#常见问题排查)
- [帧率测量](#帧率测量)

---

## 系统要求

| 组件 | 要求 |
|------|------|
| 操作系统 | Ubuntu 22.04+ / Linux（推荐） |
| CPU | 任意多核 x86_64 |
| GPU | Intel 核显 / AMD 核显 / NVIDIA 独显（任选其一）|
| FFmpeg | 系统已安装，支持对应硬件加速 |
| Node.js | 20 LTS+ |
| Rust | stable >= 1.78 |
| cmake | 编译 turbojpeg 依赖（`sudo apt install cmake`） |

### GPU 自动探测

客户端按以下顺序探测硬件加速，**自动选择最优路径**：

| 优先级 | hwaccel | MJPEG 编码器 | 适用 GPU | 说明 |
|--------|---------|-------------|----------|------|
| 1 | qsv | `mjpeg_qsv` | Intel 核显 | 全 GPU，最高效 |
| 2 | vaapi | `mjpeg_vaapi` | Intel / AMD 核显 | 全 GPU，兼容性好 |
| 3 | cuda | `mjpeg` (软件) | NVIDIA 独显 | GPU 解码 + CPU 编码 |
| 4 | auto | `mjpeg` (软件) | 任意 | FFmpeg 自动选择 |

> **日志确认**：启动后日志中 `FFmpeg MJPEG encoder started hwaccel=xxx codec=xxx` 显示实际使用的路径。

### Intel 核显配置

```bash
# 确认渲染设备存在
ls /dev/dri/renderD128

# 确认 FFmpeg 支持 VAAPI/QSV
ffmpeg -hwaccels 2>/dev/null | grep -E "vaapi|qsv"

# 确认驱动已加载
vainfo 2>&1 | head -20

# 安装驱动（如果缺失）
sudo apt install intel-media-va-driver vainfo
```

### AMD 核显配置

```bash
# 确认渲染设备存在（AMD 通常是 renderD128 或 renderD129）
ls /dev/dri/renderD*

# 确认 VAAPI 可用
ffmpeg -hwaccels 2>/dev/null | grep vaapi
vainfo 2>&1 | head -20

# 安装 Mesa VAAPI 驱动（如果缺失）
sudo apt install mesa-va-drivers vainfo
```

### NVIDIA 独显配置

```bash
# 确认 NVIDIA 驱动已安装
nvidia-smi

# 确认 CUDA 可用
ffmpeg -hwaccels 2>/dev/null | grep cuda

# 安装 NVIDIA 驱动（如果缺失）
sudo apt install nvidia-driver-535
# 或使用 Additional Drivers 工具
```

> **注意**：NVIDIA 没有硬件 MJPEG 编码器（无 `mjpeg_nvenc`），解码用 CUDA GPU 加速，编码回退到 CPU 软件。CPU 占用会比 Intel/AMD 高，但解码仍是 GPU 加速。

---

## 网络配置

IMCA Client 通过 UDP 接收选手端图传，通过 MQTT 与选手端通信。

### 竞赛网络拓扑

```
选手端 (192.168.12.1)  ←——网线直连——→  自定义客户端 (192.168.12.2)
         NIC2                                    enp67s0
```

### 配置网络接口

将自定义客户端的有线网卡设为 `192.168.12.2/24`，与选手端同一网段：

```bash
# 方法一：临时设置（重启失效）
sudo ip addr add 192.168.12.2/24 dev enp67s0
sudo ip link set enp67s0 up

# 方法二：使用项目附带脚本
bash setup-competition-network.sh
```

### 验证连通性

```bash
# 能 ping 通选手端即为正常
ping 192.168.12.1
```

### 端口说明

| 端口 | 协议 | 用途 |
|------|------|------|
| 3334 | UDP | H.265 视频流接收 |
| 3333 | TCP | MQTT 连接选手端 |

---

## 快速启动

### 1. 安装依赖

```bash
cd IMCA_Client_v1.2
npm install
```

### 2. 启动客户端（推荐配置）

```bash
# 推荐配置：1080P 60fps 低延迟
SHARK_VAAPI_SCALE_HEIGHT=1080 \
SHARK_DECODE_QUEUE=1 \
SHARK_JPEG_QUALITY=40 \
SHARK_MAX_FPS=60 \
SHARK_LOG=info \
npm run tauri dev
```

启动后：
1. 在客户端 UI 中确认 UDP 监听端口为 `3334`
2. 确认 MQTT 连接到 `192.168.12.1:3333`
3. Source Host 设为 `192.168.12.1`（只接收选手端的包）
4. 等待选手端开始推流，画面自动出现

### 3. 编译 release 版本（更高帧率）

```bash
# 编译 release（首次较慢，约 2-5 分钟）
npm run tauri build

# 直接运行 release 二进制
SHARK_VAAPI_SCALE_HEIGHT=1080 \
SHARK_DECODE_QUEUE=1 \
SHARK_JPEG_QUALITY=40 \
SHARK_MAX_FPS=60 \
SHARK_LOG=info \
./src-tauri/target/release/IMCA_Client_v1.2
```

> **注意**：`npm run dev` 会在后台占用 1420 端口。如果提示端口占用，先 `pkill -f vite` 再重试。

---

## 环境变量调参

所有参数均可通过环境变量在**启动时**设置，**无需重新编译**。改完重启 app 即可生效。

### 视频参数

| 环境变量 | 默认值 | 说明 | 调高效果 | 调低效果 |
|----------|--------|------|----------|----------|
| `SHARK_VAAPI_SCALE_HEIGHT` | 0（关） | GPU 内缩放目标高度（如 1080、720、480） | 更清晰 | CPU 更轻松 |
| `SHARK_MAX_FPS` | 60 | MJPEG 输出帧率上限 | 帧率高 | 省 CPU |
| `SHARK_DECODE_QUEUE` | 8 | H.265 解码队列深度 | 不易卡 | 延迟低 |
| `SHARK_JPEG_QUALITY` | 50 | JPEG 编码质量（1-100） | 清晰 | 带宽小 |

### 高级参数

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `SHARK_VAAPI_DEVICE` | `/dev/dri/renderD128` | VAAPI 渲染设备路径 |
| `SHARK_MJPEG_MAX_HEIGHT` | 720 | JPEG 编码前 CPU 缩放目标高度（设 0 禁用缩放） |
| `SHARK_FFMPEG_PATH` | 自动探测 | 指定 FFmpeg 二进制路径 |
| `SHARK_LOG` | `info` | 日志级别（`debug`/`info`/`warn`/`error`） |

### 调参策略

**延迟 vs 流畅度** 的核心矛盾靠 `SHARK_DECODE_QUEUE` 平衡：

| 场景 | 推荐配置 |
|------|----------|
| 竞赛操作（低延迟优先） | `SHARK_DECODE_QUEUE=1 SHARK_VAAPI_SCALE_HEIGHT=480` |
| 竞赛操作（平衡） | `SHARK_DECODE_QUEUE=1 SHARK_VAAPI_SCALE_HEIGHT=720` |
| 高清观战 | `SHARK_DECODE_QUEUE=1 SHARK_VAAPI_SCALE_HEIGHT=1080` |
| 网络丢包严重（稳定优先） | `SHARK_DECODE_QUEUE=4 SHARK_VAAPI_SCALE_HEIGHT=720` |

---

## 架构概览

### 视频流水线（v1.2 多 GPU 路径）

```
UDP 3334
    │
    ▼
┌──────────────────────┐
│  分片重组（assembly） │  8字节头 [frame# | frag# | total_size] + HEVC payload
│  接收缓冲 32MB        │  重组后送入 codec_detect
└──────────┬───────────┘
           │ Annex B HEVC 帧
           ▼
┌──────────────────────┐
│  关键帧闸门           │  等待 IDR 关键帧才开始解码，丢弃无参考的 P/B 帧
│  按需喂入解码队列     │  队列满时清空等下个 IDR，防止延迟堆积
└──────────┬───────────┘
           │
           ▼
┌──────────────────────────────────────────────┐
│  FFmpeg 子进程（自动选择最优路径）            │
│                                              │
│  路径 A (Intel QSV):                         │
│    H.265 QSV 硬解 → scale_qsv → mjpeg_qsv   │  ← 全 GPU，最高效
│                                              │
│  路径 B (Intel/AMD VAAPI):                   │
│    H.265 VAAPI 硬解 → scale_vaapi            │  ← 全 GPU，兼容性好
│    → mjpeg_vaapi GPU 硬编码 JPEG             │
│                                              │
│  路径 C (NVIDIA CUDA):                       │
│    H.265 CUDA 硬解 → hwdownload → mjpeg CPU  │  ← GPU 解码 + CPU 编码
│                                              │
│  输出 raw JPEG 帧到 stdout                   │
└──────────┬───────────────────────────────────┘
           │ raw JPEG bytes
           ▼
┌──────────────────────────────────────────────┐
│  MJPEG 读取线程                               │
│  解析 JPEG SOI/EOI 标记                       │
│  零拷贝转发到 MJPEG HTTP 流                   │
└──────────┬───────────────────────────────────┘
           │ multipart/x-mixed-replace
           ▼
┌──────────────────────┐
│  前端 <img> 标签      │  http://127.0.0.1:<port>/udp.mjpg
└──────────────────────┘
```

### 关键设计决策

1. **多 GPU 硬件编码**：自动探测 QSV → VAAPI → CUDA，Intel/AMD 显卡实现全 GPU 管线（解码+编码都在 GPU），NVIDIA 显卡 GPU 解码 + CPU 编码。1080p 60fps 稳定，码率约 8Mbps。

2. **turbojpeg SIMD 回退**：当 mjpeg_vaapi 不可用时，回退到 turbojpeg（libturbojpeg SIMD 加速）的 CPU 编码路径，比 `image` crate 快 3-5 倍。

3. **VAAPI 不杀进程重建**：每个 GOP（关键帧间隔）遇到 IDR 时直接喂给 FFmpeg，HEVC 解码器自行刷新参考帧。避免了每秒一次杀 FFmpeg 重建的 ~50ms 停顿。

4. **FFmpeg stderr 分级**：UDP 丢包导致的 `Could not find ref with POC` 是常见噪音（下一个关键帧自愈），降级到 debug 日志；真正的致命错误（spawn 失败、格式不兼容）保持 warn。

5. **大接收缓冲**：默认 16MB（请求），内核翻倍到 ~32MB。防止 IDR 大帧突发时内核丢分片。

### 关键文件

| 文件 | 职责 |
|------|------|
| `src-tauri/src/udp_bridge.rs` | UDP 接收、分片重组、MJPEG 解码路径、MJPEG 发布 |
| `src-tauri/src/udp_bridge/codec_detect.rs` | Annex B NAL 解析、H.265 关键帧检测 |
| `src-tauri/src/udp_bridge/assembly.rs` | 分片重组引擎、缓冲池 |
| `src-tauri/src/udp_bridge/mjpeg.rs` | MJPEG HTTP 流服务器 |
| `src-tauri/src/video_decoder.rs` | SmartDecoder + MjpegDecoder + turbojpeg 编码 |
| `src-tauri/src/video_decoder/gpu.rs` | FFmpeg 子进程 GPU 解码（VAAPI/QSV/CUDA）+ MJPEG 编码器 |
| `src/views/Dashboard.vue` | 前端视频显示与统计面板 |

---

## 延迟分析

v1.2 的管线延迟约 **50ms**（1080p 60fps），由以下阶段组成：

| 阶段 | 延迟 | 说明 |
|------|------|------|
| UDP 组帧 | ~0ms | 分片重组即时完成 |
| 队列等待 (QUEUE=1) | ~17ms | 1 帧缓冲 |
| FFmpeg hevc 解码 | ~16ms | hevc 格式固有 1 帧延迟 |
| mjpeg_vaapi 编码 | ~1-2ms | GPU 硬编码 |
| 浏览器渲染 | ~16ms | `<img>` 标签 1 帧延迟 |
| **总计** | **~50ms** | 接近物理极限 |

> **无法再压缩的部分**：hevc 解码器必须看到下一帧起始码才能输出当前帧（1 帧固有延迟），浏览器 `<img>` 标签有 1 帧渲染延迟。两者合计 32ms 不可避免。

---

## 常见问题排查

### 黑屏 / "等待首帧"

**症状**：客户端显示"等待首帧"，黑屏。

**排查步骤**：
```bash
# 1. 确认 UDP 包在过来
sudo timeout 3 tcpdump -i enp67s0 -n 'udp port 3334' | head
# 应该有大量包

# 2. 确认源地址过滤没挡住包
#    在 UI 中 Source Host 设为 192.168.12.1（或留空接受任何来源）

# 3. 看日志有没有 FFmpeg 报错
SHARK_LOG=info npm run tauri dev 2>&1 | grep -i "DIAG\|keyframe\|backend\|died\|error"
```

**常见原因**：
- 选手端没在推流（选手端自己也是黑屏）
- Source Host 设成了 `127.0.0.1`（挡住了远程包）
- 网线没接 / IP 不在同一网段

### 画面偶尔卡住几秒

**原因**：UDP 丢包 → 整帧丢失 → 后续 P 帧无参考 → 等下一个 IDR 关键帧恢复。GOP（关键帧间隔）越长，冻结越久。

**缓解**：
```bash
# 增大解码队列，减少因队列满丢帧
SHARK_DECODE_QUEUE=4

# 增大接收缓冲（需先放开内核上限，交给你执行）
sudo sysctl -w net.core.rmem_max=33554432
```

### MQTT 连接被拒 / BadClientId 反复刷屏

**原因**：有旧的 IMCA Client 实例没退出，两个实例用同一个 client_id 互相打架。

**解决**：
```bash
pkill -9 -f IMCA_Client
pkill -9 -f 'tauri dev'
sleep 2
# 重新启动
```

### GPU 硬解不生效 / 回退到 CPU

**排查**：
```bash
# 确认 FFmpeg 支持哪些硬件加速
ffmpeg -hwaccels 2>/dev/null

# 确认 MJPEG 硬件编码器
ffmpeg -encoders 2>/dev/null | grep mjpeg

# Intel: 确认设备和驱动
ls /dev/dri/renderD128
vainfo 2>&1 | grep -i "vaapi\|h265\|hevc"

# AMD: 确认 Mesa 驱动
ls /dev/dri/renderD*
vainfo 2>&1 | head -20

# NVIDIA: 确认驱动
nvidia-smi
```

**日志确认**：
- `hwaccel=qsv codec=mjpeg_qsv` → Intel QSV 全 GPU ✅
- `hwaccel=vaapi codec=mjpeg_vaapi` → VAAPI 全 GPU ✅（Intel/AMD）
- `hwaccel=cuda codec=mjpeg` → NVIDIA GPU 解码 + CPU 编码 ⚠️
- `hwaccel=auto codec=mjpeg` → 软件回退 ❌

如果探测结果不对，可以指定设备：
```bash
# 指定 VAAPI 设备（AMD 多 render node 时）
SHARK_VAAPI_DEVICE=/dev/dri/renderD129 npm run tauri dev
```

### 切换兵种后连接断开

**症状**：提前打开客户端并连接，切换兵种后切回来连接不上。

**原因**：选手端切换兵种时会重置网络/MQTT 连接，但客户端的 MQTT 连接状态没有自动重连。

**解决**：重新启动客户端。

### 端口 1420 被占用

```bash
fuser -k 1420/tcp 2>/dev/null
# 或
pkill -f vite
```

---

## 帧率测量

项目附带 `measure-fps.sh` 脚本，可直接测量 MJPEG 输出的真实帧率：

```bash
# 自动探测 MJPEG 端口
./measure-fps.sh

# 或手动指定（从启动日志里找）
./measure-fps.sh http://127.0.0.1:44347/udp.mjpg
```

输出示例：
```
  [14:55:00]  60 fps   (平均 59.8)
  [14:55:01]  60 fps   (平均 59.9)
```

> 帧率反映端到端真实输出（解码 → 缩放 → JPEG 编码 → 网络 → 浏览器），比 UI 统计更准确。

---

## 典型配置示例

### 竞赛模式（推荐，1080P 60fps 低延迟）

```bash
SHARK_VAAPI_SCALE_HEIGHT=1080 \
SHARK_DECODE_QUEUE=1 \
SHARK_JPEG_QUALITY=40 \
SHARK_MAX_FPS=60 \
SHARK_LOG=info \
npm run tauri dev
```

预期：~60fps，~50ms 延迟，8Mbps 码率，CPU 几乎零占用。

### 竞赛模式（720P 低带宽）

```bash
SHARK_VAAPI_SCALE_HEIGHT=720 \
SHARK_DECODE_QUEUE=1 \
SHARK_JPEG_QUALITY=40 \
SHARK_MAX_FPS=60 \
SHARK_LOG=info \
npm run tauri dev
```

预期：~60fps，~50ms 延迟，4Mbps 码率。

### 竞赛模式（480P 极低延迟）

```bash
SHARK_VAAPI_SCALE_HEIGHT=480 \
SHARK_DECODE_QUEUE=1 \
SHARK_JPEG_QUALITY=40 \
SHARK_MAX_FPS=60 \
SHARK_LOG=info \
npm run tauri dev
```

预期：~60fps，~50ms 延迟，2Mbps 码率。

### 网络丢包严重（稳定优先）

```bash
SHARK_VAAPI_SCALE_HEIGHT=720 \
SHARK_DECODE_QUEUE=4 \
SHARK_JPEG_QUALITY=40 \
SHARK_MAX_FPS=30 \
SHARK_LOG=info \
npm run tauri dev
```

预期：~30fps，~100ms 延迟，不容易因丢包卡住。
