#!/bin/bash
# ============================================================
# RoboMaster 赛事 - 自定义客户端网络配置脚本
# ============================================================
# 拓扑:
#   选手端电脑(Windows, 网卡2: 192.168.12.1) --网线--> 你的客户端(192.168.12.2)
#
# 前提(重要!):
#   选手端官方客户端必须先正常显示图传画面,
#   它才会把 HEVC 图传流转发到 192.168.12.2:3334
# ============================================================

IFACE="enp67s0"          # 你的有线网卡(连选手端)
MY_IP="192.168.12.2"     # 自定义客户端固定 IP
PEER_IP="192.168.12.1"   # 选手端电脑网卡2

echo "=========================================="
echo "  RoboMaster 自定义客户端网络配置"
echo "=========================================="

# 1. 清理可能残留的旧 IP
sudo ip addr del ${MY_IP}/24 dev $IFACE 2>/dev/null

# 2. 设置静态 IP
echo "[1] 设置 $IFACE -> $MY_IP/24"
sudo ip addr add ${MY_IP}/24 dev $IFACE
sudo ip link set $IFACE up

# 3. 验证
echo ""
echo "[2] 当前 IP:"
ip addr show $IFACE | grep "inet "

# 4. 物理链路检测
echo ""
echo "[3] 网线连接状态:"
carrier=$(cat /sys/class/net/$IFACE/carrier 2>/dev/null)
if [ "$carrier" = "1" ]; then
    echo "    ✓ 网线已连接"
else
    echo "    ✗ 网线未连接 — 检查物理连接!"
fi

# 5. ping 选手端
echo ""
echo "[4] 测试连接选手端 ($PEER_IP):"
if ping -c 3 -W 1 $PEER_IP > /dev/null 2>&1; then
    echo "    ✓ 选手端可达!网络通了"
else
    echo "    ✗ ping 不通选手端"
    echo "    检查: 1) 选手端网卡2 是否配了 $PEER_IP"
    echo "          2) 选手端是否关闭了防火墙"
    echo "          3) 网线是否接对网口"
fi

echo ""
echo "=========================================="
echo "  下一步"
echo "=========================================="
echo "1. 确认选手端官方客户端【已显示图传画面】"
echo "   (否则它不会转发视频流!)"
echo ""
echo "2. 启动你的客户端:"
echo "   cd ~/IMCA_Client_v1.2 && npm run tauri dev"
echo ""
echo "3. UDP 配置: Host=0.0.0.0  Port=3334  SourceHost=留空"
echo ""
echo "4. 还没画面?抓包确认图传有没有过来:"
echo "   sudo tcpdump -i $IFACE -n 'udp port 3334'"
echo "=========================================="
