#!/bin/bash
# ============================================================
# MJPEG 输出帧率实测脚本
# ============================================================
# 直接连接客户端后端的 MJPEG 流,统计每秒真正吐出的 JPEG 帧数。
# 这是端到端真实帧率(解码 → 缩放 → JPEG 编码 → 输出 的最终结果)。
#
# 用法:
#   ./measure-fps.sh                  # 自动探测 MJPEG 端口
#   ./measure-fps.sh http://127.0.0.1:44347/udp.mjpg   # 手动指定 URL
#
# 前提:客户端 (npm run tauri dev) 已在跑,且已连上机器人有画面。
# 按 Ctrl+C 停止。
# ============================================================

URL="$1"

# 没传 URL 就自动探测:找监听中的 MJPEG 端口
if [ -z "$URL" ]; then
    echo "[*] 未指定 URL,自动探测 MJPEG 端口..."
    # IMCA Client 的 MJPEG server 监听在 127.0.0.1 的某个随机高位端口,
    # 进程名是 IMCA_Client_v1_2。逐个端口试 /udp.mjpg。
    PORT=$(ss -tlnp 2>/dev/null \
        | grep -i 'IMCA_Client\|imca_client\|sharkclient\|SharkClient' \
        | grep -oP '127\.0\.0\.1:\K[0-9]+' \
        | head -1)

    # 退路:直接从客户端日志里也找不到时,扫所有本地监听端口试 /udp.mjpg
    if [ -z "$PORT" ]; then
        for p in $(ss -tlnp 2>/dev/null | grep -oP '127\.0\.0\.1:\K[0-9]+' | sort -u); do
            if curl -s --max-time 1 "http://127.0.0.1:${p}/udp.mjpg" -o /dev/null -w '%{content_type}' 2>/dev/null | grep -qi multipart; then
                PORT=$p
                break
            fi
        done
    fi

    if [ -z "$PORT" ]; then
        echo "[!] 自动探测失败。请从客户端启动日志里找这一行:"
        echo "      backend MJPEG stream ready url=http://127.0.0.1:XXXXX/udp.mjpg"
        echo "    然后手动运行:  ./measure-fps.sh http://127.0.0.1:XXXXX/udp.mjpg"
        exit 1
    fi
    URL="http://127.0.0.1:${PORT}/udp.mjpg"
    echo "[*] 探测到: $URL"
fi

echo "=========================================="
echo "  实测 MJPEG 输出帧率"
echo "  URL: $URL"
echo "  (Ctrl+C 停止)"
echo "=========================================="
echo ""

# curl 拉流,逐字节扫描 multipart 边界 "sharkframe"(每帧一个边界)。
# 每出现一次边界 = 收到一帧。awk 按秒聚合计数。
curl -s --no-buffer "$URL" \
| stdbuf -o0 grep -a --line-buffered -o 'sharkframe' \
| awk '
    BEGIN {
        count = 0
        last = systime()
        total = 0
        secs = 0
    }
    {
        count++
        now = systime()
        if (now != last) {
            elapsed = now - last
            # 处理跨多秒的间隔(卡顿时)
            fps = count / elapsed
            total += count
            secs += elapsed
            printf "  [%s]  %d fps   (平均 %.1f)\n", \
                   strftime("%H:%M:%S", now), int(fps + 0.5), total / secs
            count = 0
            last = now
        }
    }
'
