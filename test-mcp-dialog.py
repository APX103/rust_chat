#!/usr/bin/env python3
"""
一键 MCP 对话测试脚本
用法: python3 test-mcp-dialog.py
功能: 编译 -> 启动 mini-agent -> 自动对话测试 time/filesystem MCP -> 输出报告
"""

import os
import pty
import re
import select
import shutil
import subprocess
import sys
import time

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
CONFIG_FILE = os.path.join(SCRIPT_DIR, ".mini-agent", "config.toml")
CONFIG_BACKUP = CONFIG_FILE + ".mcp-test-backup"


def log(msg, color=None):
    colors = {
        'green': '[32m',
        'red': '[31m',
        'yellow': '[33m',
        'blue': '[34m',
        'bold': '[1m',
        'reset': '[0m',
    }
    if color:
        print(f"{colors.get(color, '')}{msg}{colors['reset']}", flush=True)
    else:
        print(msg, flush=True)


def run_build(manifest):
    log(f"编译 {os.path.basename(os.path.dirname(manifest))}...")
    result = subprocess.run(
        ["cargo", "build", "--release", "--manifest-path", manifest],
        cwd=SCRIPT_DIR,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        log(result.stdout)
        log(result.stderr)
        raise RuntimeError(f"编译失败: {manifest}")


def backup_and_patch_config():
    if not os.path.exists(CONFIG_BACKUP):
        shutil.copy2(CONFIG_FILE, CONFIG_BACKUP)
    with open(CONFIG_FILE, "r", encoding="utf-8") as f:
        content = f.read()

    # 临时注释掉需要联网的 web-search，避免测试等待
    lines = content.splitlines()
    new_lines = []
    in_web_search = False
    for line in lines:
        stripped = line.strip()
        if stripped == "[mcp_servers.web-search]":
            in_web_search = True
            new_lines.append("# " + line)
            continue
        if in_web_search:
            if stripped.startswith("[") and stripped != "[mcp_servers.web-search]":
                in_web_search = False
            else:
                new_lines.append("# " + line)
                continue
        new_lines.append(line)

    with open(CONFIG_FILE, "w", encoding="utf-8") as f:
        f.write("\n".join(new_lines))


def restore_config():
    if os.path.exists(CONFIG_BACKUP):
        shutil.copy2(CONFIG_BACKUP, CONFIG_FILE)
        os.remove(CONFIG_BACKUP)


def kill_mini_agent():
    subprocess.run(["pkill", "-9", "-f", "mini-agent"], capture_output=True)
    subprocess.run(["pkill", "-9", "-f", "mcp-time-rs"], capture_output=True)
    subprocess.run(["pkill", "-9", "-f", "mcp-filesystem-rs"], capture_output=True)


def spawn_mini_agent():
    """用伪终端启动 mini-agent，返回 (pid, master_fd)"""
    mini_agent = os.path.join(SCRIPT_DIR, "target", "release", "mini-agent")
    if not os.path.exists(mini_agent):
        raise RuntimeError(f"找不到 mini-agent: {mini_agent}")

    master_fd, slave_fd = pty.openpty()
    pid = os.fork()
    if pid == 0:
        # 子进程
        os.close(master_fd)
        os.setsid()
        os.dup2(slave_fd, 0)
        os.dup2(slave_fd, 1)
        os.dup2(slave_fd, 2)
        os.close(slave_fd)
        os.execv(mini_agent, [mini_agent])
    else:
        os.close(slave_fd)
        return pid, master_fd


def read_output(master_fd, timeout=1.0):
    """从伪终端读取可用输出"""
    output = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        remaining = deadline - time.time()
        if remaining <= 0:
            break
        ready, _, _ = select.select([master_fd], [], [], min(remaining, 0.1))
        if ready:
            try:
                chunk = os.read(master_fd, 4096)
                if chunk:
                    output += chunk
            except OSError:
                break
    return output.decode("utf-8", errors="replace")


def send_input(master_fd, text):
    os.write(master_fd, (text + "\r").encode("utf-8"))


def wait_for_pattern(master_fd, pattern, timeout=30):
    """等待输出中匹配指定正则"""
    collected = ""
    deadline = time.time() + timeout
    regex = re.compile(pattern)
    while time.time() < deadline:
        chunk = read_output(master_fd, timeout=0.5)
        if chunk:
            collected += chunk
            sys.stdout.write(chunk)
            sys.stdout.flush()
        if regex.search(collected):
            return collected
        if time.time() >= deadline:
            break
    return collected


def main():
    passed = 0
    failed = []

    log("═══ MCP 对话测试开始 ═══", "bold")
    log(f"项目目录: {SCRIPT_DIR}")

    # 1. 编译
    try:
        run_build(os.path.join(SCRIPT_DIR, "Cargo.toml"))
        run_build(os.path.join(SCRIPT_DIR, "examples", "mcp-time", "Cargo.toml"))
        run_build(os.path.join(SCRIPT_DIR, "examples", "mcp-filesystem", "Cargo.toml"))
    except RuntimeError as e:
        log(f"FAIL: {e}")
        return 1

    # 2. 准备配置
    backup_and_patch_config()

    # 3. 清理残留进程
    kill_mini_agent()
    time.sleep(0.5)

    # 4. 启动 mini-agent
    log("\n启动 mini-agent 并运行对话测试...")
    pid, master_fd = spawn_mini_agent()

    try:
        # 等待启动
        output = wait_for_pattern(master_fd, r"MCP connected\. Discovered", timeout=30)
        if "MCP connected" not in output:
            failed.append("mini-agent 启动失败或 MCP 未连接")
            return 1

        m = re.search(r"Discovered (\d+) tools", output)
        if m:
            log(f"已发现 {m.group(1)} 个 MCP 工具")
        else:
            log("已发现 MCP 工具（数量未显示）")

        wait_for_pattern(master_fd, r"Type /help for commands", timeout=5)

        # 测试 1: 时间
        log("\n测试 1: mcp_time_get_current_time")
        send_input(master_fd, "现在的时间戳是多少")
        output = wait_for_pattern(master_fd, r"🧠 You:", timeout=30)

        if "mcp_time_get_current_time" in output:
            ts_match = re.search(r"(当前系统时间戳|时间戳|timestamp).*?(\d{10,})", output, re.S | re.I)
            if ts_match:
                ts = int(ts_match.group(2))
                if 1577836800 < ts < 4102444800:
                    log(f"PASS: 时间工具调用成功，返回时间戳: {ts}")
                    passed += 1
                else:
                    failed.append("时间戳不在合理范围内")
            else:
                failed.append("时间工具返回格式不正确")
        else:
            failed.append("时间工具未被调用")

        # 测试 2: 写文件
        log("\n测试 2: mcp_filesystem_write_file")
        send_input(master_fd, "写一句话到 /tmp/mcp-auto-test.txt")
        output = wait_for_pattern(master_fd, r"🧠 You:", timeout=30)

        if "mcp_filesystem_write_file" in output and "/tmp/mcp-auto-test.txt" in output:
            log("PASS: 文件写入工具调用成功")
            passed += 1
        else:
            if "mcp_filesystem_write_file" not in output:
                failed.append("文件写入工具未被调用")
            else:
                failed.append("文件写入工具路径参数不正确")

        # 测试 3: 读文件
        log("\n测试 3: mcp_filesystem_read_file")
        send_input(master_fd, "读一下 /tmp/mcp-auto-test.txt")
        output = wait_for_pattern(master_fd, r"🧠 You:", timeout=30)

        if "mcp_filesystem_read_file" in output and "/tmp/mcp-auto-test.txt" in output:
            log("PASS: 文件读取工具调用成功")
            passed += 1
        else:
            if "mcp_filesystem_read_file" not in output:
                failed.append("文件读取工具未被调用")
            else:
                failed.append("文件读取工具路径参数不正确")

        # 测试 4: 列目录
        log("\n测试 4: mcp_filesystem_list_directory")
        send_input(master_fd, "列出 /tmp 目录里的文件")
        output = wait_for_pattern(master_fd, r"🧠 You:", timeout=30)

        if "mcp_filesystem_list_directory" in output and '"/tmp"' in output:
            log("PASS: 目录列表工具调用成功")
            passed += 1
        else:
            if "mcp_filesystem_list_directory" not in output:
                failed.append("目录列表工具未被调用")
            else:
                failed.append("目录列表工具路径参数不正确")

        # 退出
        send_input(master_fd, "/quit")
        wait_for_pattern(master_fd, r"Goodbye", timeout=5)

    finally:
        log("\n清理中...")
        os.close(master_fd)
        kill_mini_agent()
        restore_config()

    # 报告
    log("\n=== MCP 对话测试报告 ===")
    log(f"通过: {passed}", "green")
    log(f"失败: {len(failed)}", "red")
    if failed:
        log("\n失败项:")
        for f in failed:
            log(f"  - {f}")
        return 1
    else:
        log("\n全部测试通过！MCP time + filesystem 工具已可正常对话调用。")
        return 0


if __name__ == "__main__":
    sys.exit(main())
