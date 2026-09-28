# Build lshn, install it to ~\.local\bin, and run it. The Windows twin of run.sh.
# Any arguments are passed through: .\run.ps1 best, ...
$ErrorActionPreference = 'Stop'

Set-Location $PSScriptRoot
$binDir = Join-Path $HOME '.local\bin'

cargo build --release --quiet
if ($LASTEXITCODE) { exit $LASTEXITCODE }
New-Item -ItemType Directory -Force $binDir | Out-Null
Copy-Item target\release\lshn.exe $binDir -Force

& (Join-Path $binDir 'lshn.exe') @args
exit $LASTEXITCODE
