$ErrorActionPreference = "Stop"

& cargo build --release --manifest-path "$PSScriptRoot\Cargo.toml"
exit $LASTEXITCODE
