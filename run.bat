@echo off
chcp 65001 >nul
echo ╔══════════════════════════════════════╗
echo ║         Mini-Agent v0.1.0            ║
echo ║  Multi-layer memory + MCP + Skills   ║
echo ╚══════════════════════════════════════╝
echo.

if not exist "%USERPROFILE%\.mini-agent\config.toml" (
    echo 首次运行，创建默认配置...
    if not exist "%USERPROFILE%\.mini-agent" mkdir "%USERPROFILE%\.mini-agent"
    copy config.example.toml "%USERPROFILE%\.mini-agent\config.toml" >nul
    echo 请编辑 %USERPROFILE%\.mini-agent\config.toml 填入你的 API Key
    echo.
    pause
    exit /b 1
)

mini-agent.exe %*
