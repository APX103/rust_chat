#!/usr/bin/env pwsh
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

Write-Host "╔══════════════════════════════════════╗" -ForegroundColor Cyan
Write-Host "║         Mini-Agent v0.1.0            ║" -ForegroundColor Cyan
Write-Host "║  Multi-layer memory + MCP + Skills   ║" -ForegroundColor Cyan
Write-Host "╚══════════════════════════════════════╝" -ForegroundColor Cyan
Write-Host ""

$configDir = "$env:USERPROFILE\.mini-agent"
$configFile = "$configDir\config.toml"

if (-not (Test-Path $configFile)) {
    Write-Host "首次运行，创建默认配置..." -ForegroundColor Yellow
    New-Item -ItemType Directory -Force -Path $configDir | Out-Null
    Copy-Item config.example.toml $configFile
    Write-Host "请编辑 $configFile 填入你的 API Key" -ForegroundColor Yellow
    Write-Host ""
    notepad $configFile
    exit 1
}

& .\mini-agent.exe @args
