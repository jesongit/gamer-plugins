#requires -Version 5.1
[CmdletBinding()]
param([string]$OutputDir, [string]$RegistryFile)
$ErrorActionPreference = 'Stop'
$options = @{ Plugin = 'gamer-package-publisher' }
if ($OutputDir) { $options.OutputDir = $OutputDir }
if ($RegistryFile) { $options.RegistryFile = $RegistryFile }
& (Join-Path (Split-Path -Parent $PSScriptRoot) 'build.ps1') @options
