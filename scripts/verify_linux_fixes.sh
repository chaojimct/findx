#!/bin/bash
# 验证 Linux inotify 修复的手动测试脚本
#
# 使用方法：
#   chmod +x scripts/verify_linux_fixes.sh
#   ./scripts/verify_linux_fixes.sh
#
# 前提条件：
#   - Linux 系统
#   - 已编译 findx2 和 findx2-service
#   - 无 CAP_SYS_ADMIN 权限（测试 inotify 降级模式）

set -e

echo "=== FindX Linux inotify 修复验证脚本 ==="
echo ""

# 清理旧的测试目录
TEST_BASE="/tmp/findx_verify_$$"
rm -rf "$TEST_BASE"
mkdir -p "$TEST_BASE"

ROOT_A="$TEST_BASE/root_a"
ROOT_B="$TEST_BASE/root_b"
INDEX_FILE="$TEST_BASE/test.bin"

mkdir -p "$ROOT_A/subdir_a"
mkdir -p "$ROOT_B/subdir_b"

echo "测试文件 A" > "$ROOT_A/file_a.txt"
echo "测试文件 B" > "$ROOT_B/file_b.txt"

echo "✓ 创建测试环境: $TEST_BASE"
echo "  - 根目录 A: $ROOT_A"
echo "  - 根目录 B: $ROOT_B"
echo ""

# 检查两个根是否在同一设备上
DEV_A=$(stat -c '%d' "$ROOT_A")
DEV_B=$(stat -c '%d' "$ROOT_B")

if [ "$DEV_A" = "$DEV_B" ]; then
    echo "✓ 两个根目录在同一设备上 (dev: $DEV_A)"
else
    echo "⚠ 警告: 两个根目录在不同设备上 (dev_a: $DEV_A, dev_b: $DEV_B)"
    echo "  Bug #1 的测试可能不充分"
fi
echo ""

# 构建索引
echo "1. 构建多根索引..."
if command -v findx2 &> /dev/null; then
    findx2 index -o "$INDEX_FILE" --volumes "$ROOT_A,$ROOT_B"
    echo "✓ 索引构建完成: $INDEX_FILE"
else
    echo "✗ 错误: 未找到 findx2 命令"
    echo "  请先编译: cargo build --release"
    exit 1
fi
echo ""

# 检查 volume_id 唯一性（通过查看索引内容）
echo "2. 验证 volume_id 唯一性..."
if command -v strings &> /dev/null; then
    VOLUME_IDS=$(strings "$INDEX_FILE" | grep "dev:" | sort -u)
    NUM_VOLUMES=$(echo "$VOLUME_IDS" | wc -l)
    echo "  发现的 volume_id:"
    echo "$VOLUME_IDS" | sed 's/^/    /'
    if [ "$NUM_VOLUMES" -ge 2 ]; then
        echo "✓ volume_id 唯一性测试通过（找到 $NUM_VOLUMES 个不同的 volume）"
    else
        echo "✗ volume_id 唯一性测试失败（只找到 $NUM_VOLUMES 个 volume）"
        echo "  Bug #1 可能未修复"
    fi
else
    echo "⚠ 跳过: 未找到 strings 命令"
fi
echo ""

echo "3. 启动服务并测试增量更新..."
echo "  提示: 此步骤需要手动验证"
echo ""
echo "  运行以下命令启动服务:"
echo "    findx2-service --index $INDEX_FILE --volume $ROOT_A,$ROOT_B"
echo ""
echo "  然后在另一个终端执行:"
echo ""
echo "  # 测试 Bug #1: 多根路径正确性"
echo "  echo 'new file in A' > $ROOT_A/subdir_a/test_create_a.txt"
echo "  echo 'new file in B' > $ROOT_B/subdir_b/test_create_b.txt"
echo "  # 使用 IPC 客户端搜索 'test_create'，验证路径前缀正确"
echo ""
echo "  # 测试 Bug #2: 删除事件生效"
echo "  touch $ROOT_A/delete_me.txt"
echo "  sleep 1  # 等待索引"
echo "  # 搜索 'delete_me' 应找到文件"
echo "  rm $ROOT_A/delete_me.txt"
echo "  sleep 1  # 等待增量"
echo "  # 再次搜索 'delete_me' 应返回 0 结果"
echo ""

echo "4. 运行自动化测试（推荐）..."
echo "  cargo +nightly test --package findx2-linux"
echo ""

echo "=== 测试环境保留在: $TEST_BASE ==="
echo "验证完成后可手动清理: rm -rf $TEST_BASE"
