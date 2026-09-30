#requires -Version 5.1
[CmdletBinding()]
param([string]$OutputDir, [string]$RegistryFile)
$ErrorActionPreference = 'Stop'
& (Join-Path (Split-Path -Parent $PSScriptRoot) 'build.ps1') -Plugin gamer-ai -OutputDir $OutputDir -RegistryFile $RegistryFile
