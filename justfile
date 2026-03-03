# Default recipe
default:
    @just --list

# Run the app in development mode
dev:
    cargo tauri dev

# Build the app (current platform)
build:
    cargo tauri build

# Build universal macOS binary (x86_64 + aarch64)
build-macos-universal:
    cargo tauri build --target universal-apple-darwin

# Run clippy lints
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Regenerate icons from source PNG
icons:
    cargo tauri icon crates/hearkit-app/icons/source-1024.png --output crates/hearkit-app/icons

# Clean build artifacts
clean:
    cargo clean
