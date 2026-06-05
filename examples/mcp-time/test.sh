#!/bin/bash
# MCP Time 集成测试脚本
# 测试完整的 JSON-RPC MCP 会话流程

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/target/debug"
BIN="$BUILD_DIR/mcp-time-rs"

echo "=== 编译 mcp-time-rs ==="
cd "$SCRIPT_DIR"
cargo build --quiet

echo ""
echo "=== 测试 1: initialize ==="
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | "$BIN" | jq -e '.jsonrpc == "2.0" and .id == 1 and .result.protocolVersion == "2024-11-05"' && echo "✅ initialize 通过"

echo ""
echo "=== 测试 2: tools/list ==="
TOOLS=$(echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' | "$BIN")
echo "$TOOLS" | jq -e '.jsonrpc == "2.0" and .id == 2 and (.result.tools | length) == 1 and .result.tools[0].name == "get_current_time"' && echo "✅ tools/list 通过"

echo ""
echo "=== 测试 3: tools/call 返回时间戳 ==="
RESULT=$(printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_current_time","arguments":{}}}' | "$BIN")
echo "$RESULT" | jq -e '.jsonrpc == "2.0" and .id == 3 and .result.isError == false and (.result.content[0].text | startswith("Current UNIX timestamp: "))' && echo "✅ tools/call 通过"
echo "$RESULT" | jq '.result.content[0].text'

echo ""
echo "=== 测试 4: 连续会话 ==="
{
  echo '{"jsonrpc":"2.0","id":10,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'
  echo '{"jsonrpc":"2.0","id":11,"method":"tools/list"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"get_current_time","arguments":{}}}'
} | "$BIN" | jq -c '.' > /tmp/mcp-time-session.log

LINE_COUNT=$(wc -l < /tmp/mcp-time-session.log | tr -d ' ')
if [ "$LINE_COUNT" -eq 3 ]; then
  echo "✅ 连续会话返回 3 条响应"
else
  echo "❌ 连续会话返回 $LINE_COUNT 条响应，期望 3"
  exit 1
fi

grep -q '"id":10' /tmp/mcp-time-session.log && echo "✅ id=10 响应存在"
grep -q '"id":11' /tmp/mcp-time-session.log && echo "✅ id=11 响应存在"
grep -q '"id":12' /tmp/mcp-time-session.log && echo "✅ id=12 响应存在"

echo ""
echo "=============================="
echo "✅ 全部测试通过"
