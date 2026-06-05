#!/bin/bash
# MCP Filesystem 集成测试脚本
# 测试完整的 JSON-RPC MCP 会话流程

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/target/debug"
BIN="$BUILD_DIR/mcp-filesystem-rs"
TEST_DIR="/tmp/mcp-filesystem-test-$$"
mkdir -p "$TEST_DIR"
trap 'rm -rf "$TEST_DIR"' EXIT

echo "=== 编译 mcp-filesystem-rs ==="
cd "$SCRIPT_DIR"
cargo build --quiet

echo ""
echo "=== 测试 1: initialize ==="
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | "$BIN" | jq -e '.jsonrpc == "2.0" and .id == 1 and .result.protocolVersion == "2024-11-05"' && echo "✅ initialize 通过"

echo ""
echo "=== 测试 2: tools/list ==="
TOOLS=$(echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | "$BIN")
echo "$TOOLS" | jq -e '.jsonrpc == "2.0" and .id == 2 and (.result.tools | length) == 3' && echo "✅ tools/list 通过"
echo "$TOOLS" | jq '.result.tools[].name'

echo ""
echo "=== 测试 3: write_file ==="
printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"'$TEST_DIR'/hello.txt","content":"Hello from MCP!\nLine 2"}}}' | "$BIN" | jq -e '.jsonrpc == "2.0" and .id == 3 and .result.isError == false' && echo "✅ write_file 通过"
echo "写入内容："
cat "$TEST_DIR/hello.txt"

echo ""
echo "=== 测试 4: read_file ==="
printf '%s\n' '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"'$TEST_DIR'/hello.txt"}}}' | "$BIN" | jq -e '.jsonrpc == "2.0" and .id == 4 and .result.isError == false and .result.content[0].text == "Hello from MCP!\nLine 2"' && echo "✅ read_file 通过"

echo ""
echo "=== 测试 5: list_directory ==="
printf '%s\n' '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list_directory","arguments":{"path":"'$TEST_DIR'"}}}' | "$BIN" | jq -e '.jsonrpc == "2.0" and .id == 5 and .result.isError == false and (.result.content[0].text | contains("hello.txt"))' && echo "✅ list_directory 通过"

echo ""
echo "=== 测试 6: 错误处理 - 文件不存在 ==="
printf '%s\n' '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"'$TEST_DIR'/not-exist.txt"}}}' | "$BIN" | jq -e '.jsonrpc == "2.0" and .id == 6 and .result.isError == true' && echo "✅ 错误处理通过"

echo ""
echo "=== 测试 7: 未知工具 ==="
printf '%s\n' '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"unknown_tool","arguments":{}}}' | "$BIN" | jq -e '.jsonrpc == "2.0" and .id == 7 and .result.isError == true' && echo "✅ 未知工具处理通过"

echo ""
echo "=== 测试 8: 连续会话（同一个进程内）==="
{
  echo '{"jsonrpc":"2.0","id":10,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
  echo '{"jsonrpc":"2.0","id":11,"method":"tools/list"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"write_file","arguments":{"path":"'$TEST_DIR'/multi.txt","content":"multi-turn"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"'$TEST_DIR'/multi.txt"}}}'
} | "$BIN" | jq -c '.' > "$TEST_DIR/session.log"

LINE_COUNT=$(wc -l < "$TEST_DIR/session.log" | tr -d ' ')
if [ "$LINE_COUNT" -eq 4 ]; then
  echo "✅ 连续会话返回 4 条响应"
else
  echo "❌ 连续会话返回 $LINE_COUNT 条响应，期望 4"
  exit 1
fi

grep -q '"id":10' "$TEST_DIR/session.log" && echo "✅ id=10 响应存在"
grep -q '"id":11' "$TEST_DIR/session.log" && echo "✅ id=11 响应存在"
grep -q '"id":12' "$TEST_DIR/session.log" && echo "✅ id=12 响应存在"
grep -q '"id":13' "$TEST_DIR/session.log" && echo "✅ id=13 响应存在"

echo ""
echo "=============================="
echo "✅ 全部测试通过"
